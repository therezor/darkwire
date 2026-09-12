//! `RowReader` and `parse_metadata`: typed reads that name the store and the
//! column when a row is not what the schema promised.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_core::{Database, ErrorKind, GhostError, Result, RowReader, parse_metadata};
use serde_json::json;

const READ: RowReader = RowReader::new("fixture");

/// Runs `f` against the one row of a loose (non-STRICT) table, so a column can
/// hold a value of the wrong type the way a damaged file would.
fn with_row<T>(
    columns: &str,
    values: &str,
    f: impl FnOnce(&rusqlite::Row<'_>) -> Result<T>,
) -> Result<T> {
    let db = Database::in_memory().unwrap();
    db.execute_batch(&format!(
        "CREATE TABLE r ({columns}); INSERT INTO r VALUES ({values});"
    ))
    .unwrap();
    let guard = db.lock();
    let mut statement = guard.prepare("SELECT * FROM r").unwrap();
    let mut rows = statement.query([]).unwrap();
    let row = rows.next().unwrap().unwrap();
    f(row)
}

fn details(error: &GhostError, key: &str) -> String {
    error.details[key].as_str().unwrap().to_owned()
}

#[test]
fn reads_an_integer() {
    let value = with_row("n INTEGER", "42", |row| READ.int(row, "n")).unwrap();
    assert_eq!(value, 42);
}

#[test]
fn rejects_text_where_an_integer_was_promised() {
    let error = with_row("n INTEGER", "'forty-two'", |row| READ.int(row, "n")).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Storage);
    assert_eq!(details(&error, "store"), "fixture");
    assert_eq!(details(&error, "column"), "n");
    assert!(error.message.contains("an integer"));
    assert!(error.message.contains("\"n\""));
}

#[test]
fn rejects_null_where_an_integer_was_promised() {
    let error = with_row("n INTEGER", "NULL", |row| READ.int(row, "n")).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Storage);
    assert_eq!(details(&error, "column"), "n");
}

#[test]
fn rejects_a_column_that_is_not_there() {
    let error = with_row("n INTEGER", "1", |row| READ.int(row, "missing")).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Storage);
    assert_eq!(details(&error, "column"), "missing");
}

#[test]
fn reads_text() {
    let value = with_row("s TEXT", "'hello'", |row| READ.string(row, "s")).unwrap();
    assert_eq!(value, "hello");
}

#[test]
fn rejects_a_number_where_text_was_promised() {
    // No declared type, so SQLite keeps the integer rather than coercing it to
    // text the way a TEXT-affinity column would.
    let error = with_row("s", "7", |row| READ.string(row, "s")).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Storage);
    assert_eq!(details(&error, "store"), "fixture");
    assert_eq!(details(&error, "column"), "s");
    assert!(error.message.contains("text"));
}

#[test]
fn optional_int_distinguishes_null_from_zero() {
    let (null, zero, text) = with_row("a INTEGER, b INTEGER, c INTEGER", "NULL, 0, 'x'", |row| {
        Ok((
            READ.optional_int(row, "a"),
            READ.optional_int(row, "b"),
            READ.optional_int(row, "c"),
        ))
    })
    .unwrap();
    assert_eq!(null, None);
    assert_eq!(zero, Some(0));
    // Tolerant by design: the optional readers never fail a read.
    assert_eq!(text, None);
}

#[test]
fn optional_string_tolerates_null_and_a_missing_column() {
    let (null, present, missing) = with_row("a TEXT, b TEXT", "NULL, 'here'", |row| {
        Ok((
            READ.optional_string(row, "a"),
            READ.optional_string(row, "b"),
            READ.optional_string(row, "never_added"),
        ))
    })
    .unwrap();
    assert_eq!(null, None);
    assert_eq!(present.as_deref(), Some("here"));
    assert_eq!(missing, None);
}

#[test]
fn a_reader_is_copyable_and_debuggable() {
    let copy = READ;
    assert_eq!(format!("{copy:?}"), "RowReader { store: \"fixture\" }");
}

#[test]
fn parse_metadata_returns_the_object() {
    let bag = parse_metadata("{\"topicId\":42,\"tags\":[\"x\"]}");
    assert_eq!(bag["topicId"], json!(42));
    assert_eq!(bag["tags"], json!(["x"]));
}

#[test]
fn parse_metadata_falls_back_to_an_empty_bag_for_anything_else() {
    for raw in ["not json at all", "[1,2,3]", "null", "\"text\"", "7", ""] {
        assert!(parse_metadata(raw).is_empty(), "{raw}");
    }
}
