//! A model's `args` value, turned into an argv it meant.
//!
//! The schema asks for `string[]` and says so in the field description. Models
//! send a bare string anyway, and often a damaged one — observed verbatim:
//!
//! ```json
//! { "args": "[0] SUFFOLK wildfire 2024 fires reports UK US news updates\"]" }
//! ```
//!
//! That is a model half-serialising an array: an index marker at the front, a
//! stray quote and bracket at the back, and the actual query in the middle.
//! Refusing it is *correct* and it is also a wasted turn — the model reads the
//! validation error, produces a differently-broken string, and a small one
//! does that until the iteration cap. Accepting what it evidently meant costs
//! a few lines here and turns a dead end into a search.
//!
//! The order matters, and each step earns its place:
//!
//!  1. **Already an array** — the contract, and the common case.
//!  2. **A JSON array** — `"[\"--json\", \"query\"]"`. A model that stringified
//!     its argument list correctly should not be punished for the quotes.
//!  3. **A shell-ish string** — split on whitespace, honouring quotes so
//!     `--query "two words"` stays one argument. This is the fallback, and the
//!     one that has to cope with the damaged input above.
//!
//! What it deliberately does **not** do is run a shell or interpret `$`, `|`,
//! `>` or `;`. Those characters survive as literal text in whatever argument
//! they landed in. This turns a string into a list; it never turns one into a
//! pipeline, because the argv contract is what keeps the exec guard's
//! allow-list meaningful.

use std::sync::LazyLock;

use ghostai_protocol::json::js_trim;
use regex::Regex;
use serde_json::Value;

/// Bracket and quote debris from a half-serialised array, at either end.
static ARRAY_DEBRIS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^[\s\[\]"',]+|[\s\[\]"',]+$"#)
        .unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// `[0]`, `[1]:` — an index marker a model prefixed to its own argument.
static INDEX_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[\d+\]:?\s*").unwrap_or_else(|_| unreachable!("the pattern is a literal"))
});

/// Splits on whitespace, keeping quoted runs together.
///
/// Deliberately simpler than a shell: no escapes, no variable expansion, no
/// operators. A quote opens a run and the next matching quote closes it, which
/// is enough for `--flag "two words"` and cannot express anything else.
fn split_argv(input: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;

    for ch in input.chars() {
        if let Some(open) = quote {
            if ch == open {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            // An empty quoted string is still an argument, so opening a quote
            // counts as having started one even if nothing lands inside it.
            started = true;
            continue;
        }
        if ch.is_whitespace() {
            if started {
                argv.push(std::mem::take(&mut current));
            }
            current.clear();
            started = false;
            continue;
        }
        current.push(ch);
        started = true;
    }
    if started {
        argv.push(current);
    }
    argv
}

/// An array item as the text a program receives.
fn item_text(item: &Value) -> String {
    match item {
        Value::String(text) => text.clone(),
        Value::Null => "null".to_owned(),
        other => other.to_string(),
    }
}

/// Whatever the model sent, as an argv.
///
/// Never fails and never returns nothing distinguishable from empty: a value
/// this cannot make sense of becomes an empty argv, and whether that is allowed
/// is `requires_args`' decision rather than this function's.
pub fn coerce_argv(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items.iter().map(item_text).collect(),
        Value::String(text) => {
            let trimmed = js_trim(text);
            if trimmed.is_empty() {
                return Vec::new();
            }
            // A properly stringified array, which is a model that got it nearly
            // right. Anything else starting with `[` is the damaged-string path.
            if trimmed.starts_with('[')
                && let Ok(Value::Array(items)) = serde_json::from_str::<Value>(trimmed)
            {
                return items.iter().map(item_text).collect();
            }
            let unmarked = INDEX_MARKER.replace(trimmed, "");
            let cleaned = ARRAY_DEBRIS.replace_all(&unmarked, "");
            split_argv(&cleaned)
        }
        _ => Vec::new(),
    }
}
