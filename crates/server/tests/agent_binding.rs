//! Which agent runs a turn: the stored session wins, with one exception.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_server::agent_binding::agent_for_turn;

/// Everything but `deleted` resolves.
fn resolves(agent_id: &str) -> bool {
    agent_id != "deleted"
}

#[test]
fn takes_the_frames_pick_for_a_session_that_does_not_exist_yet() {
    assert_eq!(
        agent_for_turn(None, Some("reviewer"), &resolves).as_deref(),
        Some("reviewer")
    );
    assert_eq!(
        agent_for_turn(Some(""), Some("reviewer"), &resolves).as_deref(),
        Some("reviewer")
    );
}

#[test]
fn leaves_an_unbound_session_unbound_when_the_frame_names_nobody() {
    assert_eq!(agent_for_turn(None, None, &resolves), None);
}

#[test]
fn keeps_the_stored_agent_even_when_the_frame_names_another() {
    // The rule this exists for. A history built under one agent's prompt, tools
    // and permissions must not silently continue under another's — moving a
    // session is an explicit PATCH, never a side effect of a frame.
    assert_eq!(
        agent_for_turn(Some("writer"), Some("reviewer"), &resolves).as_deref(),
        Some("writer")
    );
}

#[test]
fn lets_the_frame_win_when_the_stored_agent_no_longer_resolves() {
    // The one exception. A deleted agent offers no settings to protect, so
    // outranking the operator's explicit pick would only drop them onto the
    // default while they watched themselves choose something else.
    assert_eq!(
        agent_for_turn(Some("deleted"), Some("reviewer"), &resolves).as_deref(),
        Some("reviewer")
    );
}

#[test]
fn keeps_the_stored_agent_when_neither_resolves() {
    // So the notice that follows names what the conversation actually claims
    // rather than whatever the last frame happened to carry.
    assert_eq!(
        agent_for_turn(Some("deleted"), Some("deleted"), &resolves).as_deref(),
        Some("deleted")
    );
    assert_eq!(
        agent_for_turn(Some("deleted"), None, &resolves).as_deref(),
        Some("deleted")
    );
}
