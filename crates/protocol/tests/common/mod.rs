//! Shared by the drift and fixture suites: JSON canonicalisation and a
//! readable diff.

use std::fmt::Write as _;

use serde_json::Value;

/// Integral floats become integers, so `125.0` and `125` compare equal the way
/// they are one number in JavaScript.
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

/// Every path at which two values differ, with both sides printed.
pub fn diff(path: &str, left: &Value, right: &Value, out: &mut String) {
    match (left, right) {
        (Value::Object(a), Value::Object(b)) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let next = format!("{path}/{key}");
                match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => diff(&next, x, y, out),
                    (Some(x), None) => {
                        let _ = writeln!(out, "  {next}: only on the TypeScript side: {x}");
                    }
                    (None, Some(y)) => {
                        let _ = writeln!(out, "  {next}: only on the Rust side: {y}");
                    }
                    (None, None) => {}
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (index, (x, y)) in a.iter().zip(b).enumerate() {
                diff(&format!("{path}/{index}"), x, y, out);
            }
        }
        _ if left != right => {
            let _ = writeln!(
                out,
                "  {path}:\n    TypeScript: {left}\n    Rust:       {right}"
            );
        }
        _ => {}
    }
}
