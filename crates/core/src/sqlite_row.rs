//! Reading typed values out of a row.
//!
//! Every table is `STRICT`, so a column that comes back the wrong type means
//! the row was written by something that bypassed the schema, or the file is
//! damaged. That is a `storage` error rather than a value to paper over: a
//! session whose `created_at_ms` reads as `0` because the column held text is
//! worse than a read that says so, because it is indexed, sorted and shown as
//! fact. Where a store really does want to tolerate a bad value it says so at
//! the call site with `optional_*`, which is a sentence a reviewer can disagree
//! with.

use rusqlite::Row;
use serde_json::{Map, Value};

use crate::errors::{ErrorKind, Result, WireError};

/// Typed readers bound to the store whose rows they read, so an error names it.
#[derive(Debug, Clone, Copy)]
pub struct RowReader {
    store: &'static str,
}

impl RowReader {
    /// Readers for `store`'s rows.
    pub const fn new(store: &'static str) -> RowReader {
        RowReader { store }
    }

    /// An integer column. Errors with `storage` when it is anything else.
    pub fn int(&self, row: &Row<'_>, column: &str) -> Result<i64> {
        row.get::<_, i64>(column)
            .map_err(|_| self.bad(column, "an integer"))
    }

    /// A text column. Errors with `storage` when it is anything else.
    pub fn string(&self, row: &Row<'_>, column: &str) -> Result<String> {
        row.get::<_, String>(column)
            .map_err(|_| self.bad(column, "text"))
    }

    /// An integer column that may be `NULL`.
    ///
    /// `SUM` over a column of all-`NULL` returns `NULL` rather than `0`, which is
    /// exactly the distinction the optional usage fields carry: a provider that
    /// never reported cached tokens is not a provider that reported zero.
    pub fn optional_int(&self, row: &Row<'_>, column: &str) -> Option<i64> {
        row.get::<_, Option<i64>>(column).ok().flatten()
    }

    /// A text column that may be `NULL`, or absent from an older schema.
    pub fn optional_string(&self, row: &Row<'_>, column: &str) -> Option<String> {
        row.get::<_, Option<String>>(column).ok().flatten()
    }

    fn bad(&self, column: &str, expected: &str) -> WireError {
        WireError::new(
            ErrorKind::Storage,
            format!("Expected {expected} in column \"{column}\""),
        )
        .with_detail("store", self.store)
        .with_detail("column", column)
    }
}

/// A JSON object column, as a bag.
///
/// Metadata is written by channels and extensions, so a malformed value is a
/// bug in something else. Failing the whole read over it would make one bad
/// write cost the user their conversation; an empty bag loses only the metadata.
/// That makes this the one column where tolerance is right, which is why it is
/// a separate function: the readers error, and this deliberately does not.
pub fn parse_metadata(raw: &str) -> Map<String, Value> {
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}
