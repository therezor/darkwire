//! The log record, as a line.
//!
//! The cases that matter are the ones where a formatter can lose information: a
//! line that is not JSON, a record that is not one of ours, and a field whose
//! value is an object. Losing a log line to the thing that was meant to make it
//! readable is worse than showing an ugly one.

// `allow-unwrap-in-tests` covers a `#[test]` body; the fixture helpers beside
// them are ordinary functions, and a helper that returned `Result` would make
// every case carry a `?` for a temporary directory that cannot fail in a way
// the case could act on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "fixture helpers in an integration test"
)]

use darkwire::log_line::format_log_line;
use darkwire_tui::{Palette, palette_for, strip_ansi};
use serde_json::{Value, json};

/// The identity palette, so every assertion below is about text.
fn plain() -> Palette {
    palette_for(Some(false))
}

/// The palette the CLI actually builds, so `dim` here is the bright black
/// production emits rather than the faint attribute it stopped emitting.
fn colour() -> Palette {
    palette_for(Some(true))
}

/// A record with the fields every line carries, plus whatever the case adds.
fn record(fields: &Value) -> String {
    let mut base = json!({"level": 40, "time": 1_786_007_865_399i64, "name": "ghost"});
    if let (Some(base), Some(extra)) = (base.as_object_mut(), fields.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    format!("{base}\n")
}

#[test]
fn puts_the_sentence_first_where_a_reader_looks() {
    let line = format_log_line(&record(&json!({"msg": "something happened"})), &plain());
    assert_eq!(line, "warn  something happened\n");
}

#[test]
fn keeps_the_context_after_the_sentence_rather_than_before_it() {
    // The real case: the memory warning, which is worth reading and was
    // previously buried in a hundred characters of JSON.
    let line = format_log_line(
        &record(&json!({
            "memory": "milestones",
            "file": "/w/memory/milestones.md",
            "msg": "memory has no description; skipped",
        })),
        &plain(),
    );

    assert_eq!(
        line,
        "warn  memory has no description; skipped · \
         memory=milestones file=/w/memory/milestones.md\n"
    );
}

#[test]
fn drops_the_fields_every_record_carries_and_none_identify_it_by() {
    // `time` because the line is read as it happens, `name` because it is
    // `darkwire` on every line this will ever see.
    let line = format_log_line(
        &record(&json!({"pid": 4, "hostname": "h", "msg": "hi"})),
        &plain(),
    );
    assert_eq!(line, "warn  hi\n");
}

#[test]
fn names_each_level_and_prints_an_unknown_one_as_itself() {
    for (level, word) in [(30, "info "), (50, "error "), (60, "fatal "), (35, "35 ")] {
        let line = format_log_line(&record(&json!({"level": level, "msg": "x"})), &plain());
        assert!(line.contains(word), "{level} gave {line:?}");
    }
}

#[test]
fn passes_a_line_that_is_not_json_through_untouched() {
    // A crash writes here too, and its output is not a record. Swallowing it
    // would hide the one thing worth seeing.
    let raw = "Error: connect ECONNREFUSED 127.0.0.1:11434\n";
    assert_eq!(format_log_line(raw, &plain()), raw);
}

#[test]
fn passes_a_json_line_that_is_not_a_record_through_untouched() {
    for raw in ["[1,2,3]\n", "\"a string\"\n", "null\n"] {
        assert_eq!(format_log_line(raw, &plain()), raw);
    }
}

#[test]
fn passes_a_record_with_no_message_through_as_its_json() {
    // Not one of ours — a library logging its own shape. Its JSON says more
    // than a level and a blank would.
    let line = "{\"level\":40,\"other\":1}\n";
    assert_eq!(format_log_line(line, &plain()), line);
}

#[test]
fn flattens_an_object_field_rather_than_walking_into_it() {
    // A nested `err` is one fact about the line, not several — and walking in
    // would rebuild the wall of JSON this exists to remove.
    let line = format_log_line(
        &record(&json!({"err": {"code": "ENOENT"}, "msg": "read failed"})),
        &plain(),
    );
    assert_eq!(line, "warn  read failed · err={\"code\":\"ENOENT\"}\n");
}

#[test]
fn collapses_newlines_in_a_value_so_one_record_stays_one_line() {
    let line = format_log_line(&record(&json!({"detail": "a\n  b", "msg": "x"})), &plain());
    assert_eq!(line, "warn  x · detail=a b\n");
}

#[test]
fn truncates_a_long_value_rather_than_wrapping_the_terminal() {
    let line = format_log_line(
        &record(&json!({"blob": "x".repeat(400), "msg": "x"})),
        &plain(),
    );
    assert!(line.contains('…'));
    assert!(line.len() < 200, "{} bytes", line.len());
}

#[test]
fn leaves_an_empty_line_alone() {
    assert_eq!(format_log_line("", &plain()), "");
    assert_eq!(format_log_line("\n", &plain()), "\n");
}

#[test]
fn paints_a_warning_and_an_error_differently() {
    let palette = colour();
    let warned = format_log_line(&record(&json!({"msg": "x"})), &palette);
    let failed = format_log_line(&record(&json!({"level": 50, "msg": "x"})), &palette);

    assert!(warned.contains(&palette.yellow.apply("warn")));
    assert!(failed.contains(&palette.red.apply("error")));
    assert_ne!(warned, failed);
}

#[test]
fn dims_the_context_but_not_the_sentence() {
    // The sentence is what a reader is scanning for; the fields say which thing
    // it is about, which only matters once the sentence has been read.
    let palette = colour();
    let line = format_log_line(
        &record(&json!({"file": "/w/a.md", "msg": "skipped"})),
        &palette,
    );

    assert!(line.contains("skipped"));
    assert!(line.contains(&palette.dim.apply("· file=/w/a.md")));
}

#[test]
fn says_the_level_in_words_so_colour_is_never_the_only_signal() {
    // `darkwire-tui`'s theme states this rule. Under NO_COLOR, in a pipe, or to a
    // reader who cannot tell the yellow from the red, `warn` still reads.
    let line = format_log_line(&record(&json!({"msg": "x"})), &colour());
    assert_eq!(strip_ansi(&line), "warn  x\n");
}

#[test]
fn is_byte_identical_to_the_plain_form_under_an_identity_palette() {
    // What `--no-color` and `NO_COLOR` both reduce to — one branch, not a
    // second code path that can drift.
    let line = record(&json!({"file": "/w/a.md", "msg": "skipped"}));
    assert_eq!(
        format_log_line(&line, &plain()),
        strip_ansi(&format_log_line(&line, &colour()))
    );
}
