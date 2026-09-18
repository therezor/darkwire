//! The task list as a prompt section: the runtime half, re-read every iteration.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot be built is a failing test either way"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use darkwire_agent::prompt::{ContextContributor, RuntimePromptContext, StaticPromptContext};
use darkwire_agent::tasks_contributor::{TasksContributor, render_task_section};
use darkwire_core::session_store::CreateSession;
use darkwire_core::{Database, SessionStore, SystemClock};
use darkwire_protocol::tasks::{TaskItem, TaskStatus};

fn task(text: &str, status: TaskStatus) -> TaskItem {
    TaskItem {
        text: text.to_owned(),
        status,
    }
}

fn store() -> Arc<SessionStore> {
    let next = AtomicU64::new(0);
    Arc::new(
        SessionStore::new(
            Database::in_memory().expect("an in-memory database"),
            Arc::new(SystemClock),
            Box::new(move || format!("id{}", next.fetch_add(1, Ordering::Relaxed))),
        )
        .expect("a session store"),
    )
}

fn context(session_key: &str) -> RuntimePromptContext {
    RuntimePromptContext {
        static_context: StaticPromptContext {
            workspace_root: "/tmp".to_owned(),
            workspace_id: "default".to_owned(),
            session_key: session_key.to_owned(),
            agent_id: None,
            channel: "cli".to_owned(),
        },
        iteration: 3,
        max_iterations: 40,
        now_ms: 0,
    }
}

#[test]
fn an_empty_list_is_no_section_rather_than_a_bare_heading() {
    assert_eq!(render_task_section(&[]), "");
}

#[test]
fn the_section_is_a_heading_and_one_line_per_task() {
    let section = render_task_section(&[
        task("Inspect auth", TaskStatus::Done),
        task("Update sessions", TaskStatus::Doing),
        task("Add tests", TaskStatus::Todo),
    ]);

    assert_eq!(
        section,
        "## Tasks\n\n[x] Inspect auth\n[>] Update sessions\n[ ] Add tests"
    );
}

#[test]
fn places_nothing_for_a_session_with_no_plan() {
    let store = store();
    store
        .ensure_session("web:1", CreateSession::default())
        .unwrap();

    let contributor = TasksContributor::new(Arc::clone(&store));
    assert_eq!(contributor.runtime_section(&context("web:1")), None);
}

#[test]
fn places_nothing_for_a_session_that_does_not_exist() {
    let contributor = TasksContributor::new(store());
    assert_eq!(contributor.runtime_section(&context("nowhere")), None);
}

#[test]
fn places_the_list_a_session_holds() {
    let store = store();
    store
        .set_tasks("web:1", &[task("Add tests", TaskStatus::Doing)])
        .unwrap();

    let contributor = TasksContributor::new(Arc::clone(&store));
    assert_eq!(
        contributor.runtime_section(&context("web:1")),
        Some("## Tasks\n\n[>] Add tests".to_owned())
    );
}

/// The reason it is the runtime half and not the static one: the list moves
/// while a turn is running, and a section read once per turn would show the
/// model a plan it had already passed.
#[test]
fn re_reads_on_every_call_rather_than_caching_what_it_saw() {
    let store = store();
    let contributor = TasksContributor::new(Arc::clone(&store));

    store
        .set_tasks("web:1", &[task("Add tests", TaskStatus::Todo)])
        .unwrap();
    assert_eq!(
        contributor.runtime_section(&context("web:1")),
        Some("## Tasks\n\n[ ] Add tests".to_owned())
    );

    store
        .set_tasks("web:1", &[task("Add tests", TaskStatus::Done)])
        .unwrap();
    assert_eq!(
        contributor.runtime_section(&context("web:1")),
        Some("## Tasks\n\n[x] Add tests".to_owned())
    );
}

/// One contributor serves every session on an agent, so the key it is asked
/// about is the only thing that may decide what it returns.
#[test]
fn answers_for_the_session_it_is_asked_about() {
    let store = store();
    store
        .set_tasks("web:1", &[task("mine", TaskStatus::Todo)])
        .unwrap();
    store
        .set_tasks("web:2", &[task("theirs", TaskStatus::Todo)])
        .unwrap();

    let contributor = TasksContributor::new(Arc::clone(&store));
    assert!(
        contributor
            .runtime_section(&context("web:2"))
            .expect("a section")
            .contains("theirs")
    );
}

#[test]
fn contributes_nothing_to_the_cached_half() {
    let store = store();
    store
        .set_tasks("web:1", &[task("Add tests", TaskStatus::Todo)])
        .unwrap();

    let contributor = TasksContributor::new(store);
    let static_context = context("web:1").static_context;
    let section = futures::executor::block_on(contributor.static_section(&static_context));
    assert_eq!(section, None);
}
