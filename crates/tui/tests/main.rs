//! Every integration test for this crate, as one binary.
//!
//! One binary per crate rather than one per file: each binary links the whole
//! dependency tree, so a file each cost minutes of linking and gigabytes of
//! `target/` for no isolation that nextest does not already give per test.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that does not hold is a failing test either way"
)]

mod common;

mod ansi;
mod editor;
mod frame;
mod history_cell;
mod insert_history;
mod keys;
mod pages;
mod renderable;
mod select;
mod select_list;
mod spinner;
mod terminal;
mod text;
mod theme;
mod tui;
mod wrap;
