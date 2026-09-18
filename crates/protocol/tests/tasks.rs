//! The task list in a session's metadata bag, and how it is read back.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_protocol::json::Object;
use darkwire_protocol::tasks::{
    MAX_TASKS, TASKS_METADATA_KEY, TaskItem, TaskStatus, render_tasks, task_counts, task_list_of,
    tasks_of, with_tasks,
};
use serde_json::{Value, json};

fn bag(value: &Value) -> Object {
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn task(text: &str, status: TaskStatus) -> TaskItem {
    TaskItem {
        text: text.to_owned(),
        status,
    }
}

#[test]
fn reads_a_list_back_in_the_order_it_was_written() {
    let tasks = tasks_of(&bag(&json!({"tasks": {"seq": 7, "items": [
        {"text": "Inspect auth", "status": "done"},
        {"text": "Update sessions", "status": "doing"},
        {"text": "Add tests", "status": "todo"},
    ]}})));

    assert_eq!(
        tasks,
        vec![
            task("Inspect auth", TaskStatus::Done),
            task("Update sessions", TaskStatus::Doing),
            task("Add tests", TaskStatus::Todo),
        ]
    );
}

#[test]
fn a_bag_with_no_list_has_no_tasks() {
    assert_eq!(tasks_of(&bag(&json!({}))), Vec::new());
    assert_eq!(tasks_of(&bag(&json!({"tasks": "later"}))), Vec::new());
    assert_eq!(tasks_of(&bag(&json!({"tasks": {"seq": 4}}))), Vec::new());
}

/// The position is what a truncation compares against, so a bag that lost it
/// reads as zero: no cut is ever below zero, so a list nobody stamped survives
/// rather than vanishing on the next edit.
#[test]
fn a_list_with_no_position_reads_as_position_zero() {
    let list = task_list_of(&bag(&json!({"tasks": {"items": [
        {"text": "Add tests", "status": "todo"},
    ]}})));
    assert_eq!(list.seq, 0);
    assert_eq!(list.items.len(), 1);
}

#[test]
fn reads_the_position_the_list_was_written_at() {
    let list = task_list_of(&bag(&json!({"tasks": {"seq": 7, "items": [
        {"text": "Add tests", "status": "todo"},
    ]}})));
    assert_eq!(list.seq, 7);
}

/// The bag is untyped storage other things also write, so one malformed entry
/// must not stop a panel rendering.
#[test]
fn skips_what_is_not_a_task_and_keeps_what_is() {
    let tasks = tasks_of(&bag(&json!({"tasks": {"seq": 1, "items": [
        "Inspect auth",
        {"status": "done"},
        {"text": ""},
        {"text": "Add tests", "status": "kicked-off"},
    ]}})));

    assert_eq!(tasks, vec![task("Add tests", TaskStatus::Todo)]);
}

/// A bag written by hand is not bound by the tool's schema, and a prompt section
/// is not the place to find that out.
#[test]
fn applies_the_cap_on_the_way_out_as_well() {
    let raw: Vec<Value> = (0..25)
        .map(|index| json!({"text": format!("task {index}"), "status": "todo"}))
        .collect();
    assert_eq!(
        tasks_of(&bag(&json!({"tasks": {"seq": 1, "items": raw}}))).len(),
        MAX_TASKS
    );
}

#[test]
fn writing_a_list_leaves_the_rest_of_the_bag_alone() {
    let next = with_tasks(
        &bag(&json!({"subagentRuns": {"call-1": {"sessionKey": "s"}}})),
        7,
        &[task("Inspect auth", TaskStatus::Doing)],
    );

    assert!(next.contains_key("subagentRuns"));
    assert_eq!(
        tasks_of(&next),
        vec![task("Inspect auth", TaskStatus::Doing)]
    );
}

#[test]
fn writing_a_list_records_where_it_was_written() {
    let next = with_tasks(&bag(&json!({})), 7, &[task("x", TaskStatus::Todo)]);
    assert_eq!(task_list_of(&next).seq, 7);
}

/// A cleared list and a session that never had one are the same row.
#[test]
fn writing_an_empty_list_removes_the_key() {
    let next = with_tasks(
        &bag(&json!({"tasks": {"seq": 7, "items": [{"text": "x", "status": "todo"}]}})),
        9,
        &[],
    );
    assert!(!next.contains_key(TASKS_METADATA_KEY));
}

#[test]
fn renders_one_line_per_task_with_the_marker_in_front() {
    let rendered = render_tasks(&[
        task("Inspect auth", TaskStatus::Done),
        task("Update sessions", TaskStatus::Doing),
        task("Add tests", TaskStatus::Todo),
    ]);

    assert_eq!(
        rendered,
        "[x] Inspect auth\n[>] Update sessions\n[ ] Add tests"
    );
}

#[test]
fn renders_nothing_for_an_empty_list() {
    assert_eq!(render_tasks(&[]), "");
}

#[test]
fn counts_each_state() {
    let counts = task_counts(&[
        task("a", TaskStatus::Done),
        task("b", TaskStatus::Done),
        task("c", TaskStatus::Doing),
        task("d", TaskStatus::Todo),
    ]);
    assert_eq!(counts, (2, 1, 1));
}

#[test]
fn a_status_round_trips_through_its_wire_spelling() {
    for status in [TaskStatus::Todo, TaskStatus::Doing, TaskStatus::Done] {
        assert_eq!(TaskStatus::parse(status.as_str()), Some(status));
    }
    assert_eq!(TaskStatus::parse("blocked"), None);
}
