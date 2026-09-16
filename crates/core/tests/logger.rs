//! The JSON logger and its redaction paths.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod capture;

use std::collections::HashMap;
use std::sync::Arc;

use capture::{Captured, NOW, capturing, options};
use darkwire_core::logger::{
    LogLevel, LoggerOptions, REDACT_CENSOR, REDACT_PATHS, create_logger, redact, resolve_level,
    silent_logger, subscriber,
};
use serde_json::{Map, Value, json};
use tracing_subscriber::layer::SubscriberExt as _;

/// The list as the TypeScript logger declares it, copied rather than imported
/// so a change on either side fails here.
const TS_REDACT_PATHS: [&str; 31] = [
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

fn log_with(sink: &Arc<Captured>, level: LogLevel, work: impl FnOnce()) -> Vec<Map<String, Value>> {
    tracing::subscriber::with_default(capturing(sink, level), work);
    sink.lines()
}

fn one_line(level: LogLevel, work: impl FnOnce()) -> Map<String, Value> {
    let sink = Captured::new();
    let mut lines = log_with(&sink, level, work);
    assert_eq!(lines.len(), 1, "{lines:?}");
    lines.remove(0)
}

mod lines {
    use super::*;

    #[test]
    fn emits_structured_json() {
        let line = one_line(LogLevel::Info, || {
            tracing::info!(tool = "read_file", "executing");
        });
        assert_eq!(line["msg"], "executing");
        assert_eq!(line["tool"], "read_file");
        assert_eq!(line["level"], 30);
    }

    #[test]
    fn tags_lines_with_a_component_name_and_base_fields() {
        let sink = Captured::new();
        let mut base = Map::new();
        base.insert("sessionKey".to_owned(), Value::from("web:1"));
        let layer_options = LoggerOptions {
            name: Some("agent".to_owned()),
            base,
            ..options(&sink, Some(LogLevel::Info))
        };
        tracing::subscriber::with_default(subscriber(layer_options), || {
            tracing::info!("one");
            tracing::info!("two");
        });
        let lines = sink.lines();
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line["name"] == "agent"));
        assert!(lines.iter().all(|line| line["sessionKey"] == "web:1"));
    }

    #[test]
    fn stamps_epoch_milliseconds_from_the_clock_and_names_the_process() {
        let line = one_line(LogLevel::Info, || tracing::info!("x"));
        assert_eq!(line["time"], NOW);
        assert_eq!(line["pid"], u64::from(std::process::id()));
        assert_eq!(line["hostname"], "test-host");
    }

    #[test]
    fn resolves_the_host_name_when_none_is_given() {
        let sink = Captured::new();
        let layer_options = LoggerOptions {
            hostname: None,
            ..options(&sink, Some(LogLevel::Info))
        };
        tracing::subscriber::with_default(subscriber(layer_options), || tracing::info!("x"));
        assert!(sink.lines()[0]["hostname"].as_str().is_some());
    }

    #[test]
    fn carries_numbers_booleans_and_errors() {
        let error = std::io::Error::other("disk on fire");
        let line = one_line(LogLevel::Info, || {
            tracing::info!(
                tokens = 42_u64,
                delta = -1_i64,
                ratio = 0.5_f64,
                ok = true,
                error = &error as &dyn std::error::Error,
                "x"
            );
        });
        assert_eq!(line["tokens"], 42);
        assert_eq!(line["delta"], -1);
        assert_eq!(line["ratio"], 0.5);
        assert_eq!(line["ok"], true);
        assert_eq!(line["error"], "disk on fire");
    }

    #[test]
    fn keeps_a_debug_value_as_a_string_unless_it_is_json() {
        let value = json!({"nested": {"n": 1}});
        let line = one_line(LogLevel::Info, || {
            tracing::info!(shape = ?vec![1, 2], structured = %value, plain = ?"text", "x");
        });
        assert_eq!(line["shape"], json!([1, 2]));
        assert_eq!(line["structured"], json!({"nested": {"n": 1}}));
        assert_eq!(line["plain"], "\"text\"");
    }

    #[test]
    fn a_message_that_looks_like_json_stays_a_message() {
        let line = one_line(LogLevel::Info, || tracing::info!("{{}}"));
        assert_eq!(line["msg"], "{}");
    }

    #[test]
    fn defaults_to_info_and_drops_debug() {
        let sink = Captured::new();
        let layer_options = LoggerOptions {
            level: None,
            env: Some(HashMap::new()),
            ..options(&sink, None)
        };
        tracing::subscriber::with_default(subscriber(layer_options), || {
            tracing::debug!("invisible");
            tracing::info!("visible");
        });
        assert_eq!(sink.messages(), ["visible"]);
    }

    #[test]
    fn reads_the_level_from_the_environment() {
        let env = HashMap::from([("DARKWIRE_LOG_LEVEL".to_owned(), "debug".to_owned())]);
        assert_eq!(resolve_level(None, &env), LogLevel::Debug);
        let fallback = HashMap::from([("LOG_LEVEL".to_owned(), "debug".to_owned())]);
        assert_eq!(resolve_level(None, &fallback), LogLevel::Debug);

        let sink = Captured::new();
        let layer_options = LoggerOptions {
            level: None,
            env: Some(env),
            ..options(&sink, None)
        };
        tracing::subscriber::with_default(subscriber(layer_options), || tracing::debug!("d"));
        assert_eq!(sink.messages(), ["d"]);
    }

    #[test]
    fn prefers_an_explicit_level_over_the_environment() {
        let env = HashMap::from([("DARKWIRE_LOG_LEVEL".to_owned(), "debug".to_owned())]);
        assert_eq!(resolve_level(Some(LogLevel::Error), &env), LogLevel::Error);
        let sink = Captured::new();
        let lines = log_with(&sink, LogLevel::Error, || {
            tracing::info!("dropped");
            tracing::error!("kept");
        });
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["level"], 50);
    }

    #[test]
    fn survives_an_unrecognised_level_rather_than_failing_at_boot() {
        let env = HashMap::from([("LOG_LEVEL".to_owned(), "chatty".to_owned())]);
        assert_eq!(resolve_level(None, &env), LogLevel::Info);
    }

    #[test]
    fn spells_and_numbers_every_level() {
        for (level, name, number) in [
            (LogLevel::Trace, "trace", 10),
            (LogLevel::Debug, "debug", 20),
            (LogLevel::Info, "info", 30),
            (LogLevel::Warn, "warn", 40),
            (LogLevel::Error, "error", 50),
            (LogLevel::Fatal, "fatal", 60),
        ] {
            assert_eq!(LogLevel::parse(name), Some(level));
            assert_eq!(level.as_str(), name);
            assert_eq!(level.number(), number);
        }
        assert_eq!(LogLevel::parse("silent"), Some(LogLevel::Silent));
        assert_eq!(LogLevel::Silent.as_str(), "silent");
    }

    #[test]
    fn numbers_every_tracing_level() {
        let sink = Captured::new();
        let lines = log_with(&sink, LogLevel::Trace, || {
            tracing::trace!("t");
            tracing::debug!("d");
            tracing::info!("i");
            tracing::warn!("w");
            tracing::error!("e");
        });
        let numbers: Vec<u64> = lines
            .iter()
            .map(|line| line["level"].as_u64().unwrap())
            .collect();
        assert_eq!(numbers, [10, 20, 30, 40, 50]);
    }

    #[test]
    fn fatal_and_silent_drop_everything() {
        let sink = Captured::new();
        log_with(&sink, LogLevel::Fatal, || tracing::error!("e"));
        log_with(&sink, LogLevel::Silent, || tracing::error!("e"));
        assert!(sink.lines().is_empty());
    }

    #[test]
    fn the_layer_composes_with_a_registry_by_hand() {
        let sink = Captured::new();
        let layer = create_logger(options(&sink, Some(LogLevel::Info)));
        assert!(format!("{layer:?}").contains("JsonLayer"));
        let composed = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(composed, || tracing::info!("hi"));
        assert_eq!(sink.messages(), ["hi"]);
    }

    #[test]
    fn options_have_a_debug_form_without_the_sink() {
        let text = format!("{:?}", options(&Captured::new(), Some(LogLevel::Info)));
        assert!(text.contains("LoggerOptions"));
        assert!(text.contains("test-host"));
    }
}

mod redaction {
    use super::*;

    #[test]
    fn the_path_list_is_exactly_the_typescript_one() {
        assert_eq!(REDACT_PATHS, TS_REDACT_PATHS);
        assert_eq!(REDACT_CENSOR, "[redacted]");
    }

    /// Builds an object holding `secret` at `path`, with `x` standing in for
    /// every wildcard.
    fn holding(path: &str) -> (Value, Vec<String>) {
        let keys: Vec<String> = path
            .replace("[\"", ".")
            .replace("\"]", "")
            .split('.')
            .map(|segment| {
                if segment == "*" {
                    "x".to_owned()
                } else {
                    segment.to_owned()
                }
            })
            .collect();
        let mut value = Value::from("secret");
        for key in keys.iter().rev() {
            value = json!({ key.as_str(): value });
        }
        (value, keys)
    }

    fn at<'a>(value: &'a Value, keys: &[String]) -> &'a Value {
        keys.iter().fold(value, |node, key| &node[key])
    }

    #[test]
    fn redacts_every_path_in_the_table() {
        for path in TS_REDACT_PATHS {
            let (mut value, keys) = holding(path);
            redact(&mut value);
            assert_eq!(at(&value, &keys), REDACT_CENSOR, "path {path}");
        }
    }

    #[test]
    fn a_wildcard_reaches_into_arrays_too() {
        let mut value = json!({"providers": [{"apiKey": "sk-1"}, {"apiKey": "sk-2"}]});
        redact(&mut value);
        assert_eq!(
            value,
            json!({"providers": [{"apiKey": REDACT_CENSOR}, {"apiKey": REDACT_CENSOR}]})
        );
    }

    #[test]
    fn leaves_a_path_that_is_not_an_object_alone() {
        let mut value = json!({"headers": "not an object", "req": 4});
        redact(&mut value);
        assert_eq!(value, json!({"headers": "not an object", "req": 4}));
    }

    #[test]
    fn redacts_a_top_level_secret() {
        let line = one_line(LogLevel::Info, || {
            tracing::info!(apiKey = "sk-live-123", "x");
        });
        assert_eq!(line["apiKey"], REDACT_CENSOR);
    }

    #[test]
    fn redacts_one_level_down() {
        let provider = json!({"apiKey": "sk-1"});
        let line = one_line(LogLevel::Info, || tracing::info!(provider = %provider, "x"));
        assert_eq!(line["provider"], json!({"apiKey": REDACT_CENSOR}));
    }

    #[test]
    fn redacts_inside_a_keyed_record_of_providers() {
        let providers = json!({"openai": {"apiKey": "sk-1"}});
        let line = one_line(
            LogLevel::Info,
            || tracing::info!(providers = %providers, "x"),
        );
        assert_eq!(
            line["providers"],
            json!({"openai": {"apiKey": REDACT_CENSOR}})
        );
    }

    #[test]
    fn redacts_request_headers() {
        let headers = json!({"authorization": "Bearer abc", "content-type": "application/json", "set-cookie": "a=b"});
        let line = one_line(LogLevel::Info, || tracing::info!(headers = %headers, "x"));
        assert_eq!(
            line["headers"],
            json!({"authorization": REDACT_CENSOR, "content-type": "application/json", "set-cookie": REDACT_CENSOR})
        );
    }

    #[test]
    fn covers_the_common_secret_field_names() {
        let line = one_line(LogLevel::Info, || {
            tracing::info!(
                token = "a",
                accessToken = "b",
                refreshToken = "c",
                password = "d",
                secret = "e",
                clientSecret = "f",
                cookie = "g",
                "x"
            );
        });
        for field in [
            "token",
            "accessToken",
            "refreshToken",
            "password",
            "secret",
            "clientSecret",
            "cookie",
        ] {
            assert_eq!(line[field], REDACT_CENSOR, "{field}");
        }
    }

    #[test]
    fn leaves_ordinary_fields_untouched() {
        let line = one_line(LogLevel::Info, || {
            tracing::info!(model = "qwen3", tokens = 42, "x");
        });
        assert_eq!(line["model"], "qwen3");
        assert_eq!(line["tokens"], 42);
    }

    #[test]
    fn cannot_reach_a_secret_interpolated_into_the_message() {
        // Documents why the convention is to log structured context: redaction
        // is by path, and a message string has no paths.
        let line = one_line(LogLevel::Info, || tracing::info!("key=sk-live-123"));
        assert_eq!(line["msg"], "key=sk-live-123");
    }
}

mod silent {
    use super::*;

    #[test]
    fn accepts_calls_and_writes_nothing() {
        tracing::subscriber::with_default(silent_logger(), || {
            tracing::error!(apiKey = "x", "ignored");
            tracing::info!("ignored");
        });
    }
}
