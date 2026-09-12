//! Shared by every suite here: JSON canonicalisation, and a turn harness.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    dead_code,
    reason = "a fixture that cannot load is a failing test either way, and each \
              suite uses a different part of the harness"
)]

use serde_json::Value;

/// Integral floats become integers, so `400.0` and `400` compare equal the way
/// they are one number in JavaScript — which is what the frame fixtures were
/// written by.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "integral, below 2^53"
)]
pub fn canonical_numbers(value: Value) -> Value {
    match value {
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 => {
                if f < 0.0 {
                    Value::from(f as i64)
                } else {
                    Value::from(f as u64)
                }
            }
            _ => Value::Number(n),
        },
        Value::Array(items) => Value::Array(items.into_iter().map(canonical_numbers).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, canonical_numbers(v)))
                .collect(),
        ),
        other => other,
    }
}

pub mod harness;
