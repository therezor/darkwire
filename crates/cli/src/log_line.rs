//! A log record, as one line a person can read.
//!
//! The logger writes JSON because that is what a log is for — `darkwire serve
//! 2>darkwire.log` produces something `jq` can answer questions about, and every
//! field is there on purpose. In a chat window it is the wrong shape entirely:
//! a wall of `{"level":40,"time":1786007865399,…}` between two turns says
//! nothing at a glance and buries the one part that matters, which is the
//! sentence.
//!
//! So the JSON is what gets written, and this is what gets *shown* — and only
//! when the stream is a terminal this process is drawing into. A redirected
//! stderr still receives the record verbatim, because the thing reading it then
//! is a program.
//!
//! **A line that cannot be parsed is passed through unchanged.** This runs over
//! whatever reaches the destination, and losing a log line to a formatter is a
//! worse failure than showing an ugly one — including for the case that matters
//! most, a crash whose output is not a record at all.

use darkwire_tui::Palette;
use serde_json::Value;

/// The numeric levels the logger writes. Anything else prints as the number it
/// was.
const LEVELS: [(i64, &str); 6] = [
    (10, "trace"),
    (20, "debug"),
    (30, "info"),
    (40, "warn"),
    (50, "error"),
    (60, "fatal"),
];

/// Fields every record carries, which say nothing a reader wants.
///
/// `time` goes because the line is being read as it happens; `pid` and
/// `hostname` because there is one process and it is this one; `name` because
/// it is `darkwire` on every line this formatter will ever see.
const NOISE: [&str; 6] = ["level", "time", "pid", "hostname", "name", "msg"];

/// How much of one field's value survives. Long enough to identify, not to
/// wrap.
pub const MAX_VALUE_CHARS: usize = 120;

/// How each level is painted.
///
/// **The word stays whatever the colour does.** `darkwire-tui`'s theme states
/// the rule this follows — colour is never the only signal — so `warn` reads as
/// `warn` under `NO_COLOR`, in a pipe, and to anyone who cannot tell the yellow
/// from the red. The colour is what makes it findable while scrolling, not what
/// makes it legible.
///
/// The low levels are dimmed rather than left plain: at `--verbose` they are
/// the bulk of what arrives, and they are the half a reader is skimming past to
/// find the one line that matters.
fn paint_level(palette: &Palette, level: &str) -> String {
    match level {
        "fatal" | "error" => palette.red.apply(level),
        "warn" => palette.yellow.apply(level),
        "trace" | "debug" => palette.dim.apply(level),
        other => other.to_owned(),
    }
}

/// One record as a line, or the line unchanged when it is not one.
///
/// The three ways out without formatting are all deliberate: a line that is not
/// JSON, a JSON value that is not an object, and an object with no `msg`. The
/// last is a record from something that is not this program logging its own
/// shape, and its JSON says more than a level and a blank would.
#[must_use]
pub fn format_log_line(line: &str, palette: &Palette) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return line.to_owned();
    }

    let Ok(Value::Object(record)) = serde_json::from_str::<Value>(trimmed) else {
        return line.to_owned();
    };

    let message = match record.get("msg") {
        Some(Value::String(text)) if !text.is_empty() => text.as_str(),
        _ => return line.to_owned(),
    };

    let label = match record.get("level") {
        Some(Value::Number(number)) => number.as_i64().map_or_else(
            || number.to_string(),
            |level| {
                LEVELS
                    .iter()
                    .find(|(value, _)| *value == level)
                    .map_or_else(|| level.to_string(), |(_, name)| (*name).to_owned())
            },
        ),
        _ => "log".to_owned(),
    };

    let context = record
        .iter()
        .filter(|(key, _)| !NOISE.contains(&key.as_str()))
        .map(|(key, value)| format!("{key}={}", render(value)))
        .collect::<Vec<_>>()
        .join(" ");

    let head = format!("{}  {message}", paint_level(palette, &label));
    if context.is_empty() {
        format!("{head}\n")
    } else {
        // The context is dimmed as a whole: it is what identifies *which* thing
        // the sentence is about, which matters only once the sentence has been
        // read.
        format!("{head} {}\n", palette.dim.apply(&format!("· {context}")))
    }
}

/// One field's value, flattened.
///
/// An object is re-serialised rather than spread, because a nested `err` is one
/// fact about the line and not several — and a formatter that walked into it
/// would reintroduce the wall of JSON this exists to avoid.
fn render(value: &Value) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let flat = collapse_whitespace(&text);
    truncate(&flat)
}

/// Every run of whitespace as one space, so one record stays one line.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
            continue;
        }
        in_space = false;
        out.push(ch);
    }
    out
}

/// The value cut to [`MAX_VALUE_CHARS`], marked when it was.
///
/// Measured in UTF-16 units, which is the unit the TypeScript logger's own cap
/// counted in, so the same record truncates at the same point in both. The cut
/// lands on a character boundary rather than mid-pair: a lone surrogate is not
/// something a terminal can draw, and the budget is a readability limit rather
/// than a wire constraint.
fn truncate(text: &str) -> String {
    if text.encode_utf16().count() <= MAX_VALUE_CHARS {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut units = 0usize;
    for ch in text.chars() {
        let width = ch.len_utf16();
        if units + width > MAX_VALUE_CHARS - 1 {
            break;
        }
        units += width;
        out.push(ch);
    }
    out.push('…');
    out
}
