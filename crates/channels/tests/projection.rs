//! Which hub events become text on a chat transport, and which are dropped.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot be built is a failing test either way"
)]

mod common;

use common::{
    SESSION, TURN, approval_request, context_usage, delta, error, nested_delta, nested_turn_end,
    nested_turn_start, notice, queued, reasoning, session_status, subagent, tool_call, tool_result,
    turn_end, turn_start,
};
use darkwire_channels::projection::{
    APPROVAL_METADATA_KEY, ApprovalDraftDetail, OutboundDraft, TurnProjection,
    TurnProjectionOptions,
};
use darkwire_core::message_bus::OutboundKind;
use darkwire_protocol::{ServerMessage, StopReason};

fn hints() -> TurnProjectionOptions {
    TurnProjectionOptions {
        send_progress: true,
        send_tool_hints: true,
    }
}

fn quiet() -> TurnProjectionOptions {
    TurnProjectionOptions {
        send_progress: false,
        send_tool_hints: false,
    }
}

/// Everything the projection says across a run of events.
fn project_all(projection: &mut TurnProjection, events: &[ServerMessage]) -> Vec<OutboundDraft> {
    events
        .iter()
        .flat_map(|event| projection.project(event))
        .collect()
}

fn texts(drafts: &[OutboundDraft]) -> Vec<String> {
    drafts.iter().map(|draft| draft.text.clone()).collect()
}

fn kinds(drafts: &[OutboundDraft]) -> Vec<OutboundKind> {
    drafts.iter().map(|draft| draft.kind).collect()
}

// The answer

#[test]
fn emits_one_reply_carrying_the_whole_answer() {
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = project_all(
        &mut projection,
        &[
            turn_start(),
            delta("Hello"),
            delta(", world"),
            turn_end(StopReason::Complete),
        ],
    );

    assert_eq!(kinds(&drafts), vec![OutboundKind::Reply]);
    assert_eq!(texts(&drafts), vec!["Hello, world"]);
    assert_eq!(drafts[0].turn_id.as_deref(), Some(TURN));
}

#[test]
fn never_emits_the_reasoning_stream() {
    let mut projection = TurnProjection::new(hints());

    let drafts = project_all(
        &mut projection,
        &[
            turn_start(),
            reasoning("the model's scratchpad"),
            delta("the answer"),
            turn_end(StopReason::Complete),
        ],
    );

    assert!(
        !texts(&drafts).join("\n").contains("scratchpad"),
        "a model's reasoning must never reach a chat app: {:?}",
        texts(&drafts)
    );
    assert_eq!(texts(&drafts), vec!["the answer"]);
}

#[test]
fn accumulates_only_the_running_turn() {
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    project_all(
        &mut projection,
        &[turn_start(), delta("first"), turn_end(StopReason::Complete)],
    );
    assert_eq!(projection.answer(), "");

    projection.project(&turn_start());
    projection.project(&delta("second"));
    assert_eq!(projection.answer(), "second");
}

#[test]
fn drops_the_answer_at_a_turn_boundary() {
    // Otherwise the next reply is a transcript of everything before it.
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    project_all(
        &mut projection,
        &[turn_start(), delta("first"), turn_end(StopReason::Complete)],
    );
    let second = project_all(
        &mut projection,
        &[
            turn_start(),
            delta("second"),
            turn_end(StopReason::Complete),
        ],
    );

    assert_eq!(texts(&second), vec!["second"]);
}

// progress and tool hints

#[test]
fn sends_the_answer_so_far_at_a_tool_boundary_when_progress_is_on() {
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = project_all(
        &mut projection,
        &[turn_start(), delta("so far"), tool_call("c1", "read")],
    );

    assert_eq!(kinds(&drafts), vec![OutboundKind::Progress]);
    assert_eq!(texts(&drafts), vec!["so far"]);
}

#[test]
fn sends_no_progress_before_the_model_has_written_anything() {
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = project_all(&mut projection, &[turn_start(), tool_call("c1", "read")]);

    assert!(drafts.is_empty());
}

#[test]
fn sends_no_progress_when_it_is_switched_off() {
    let mut projection = TurnProjection::new(quiet());

    let drafts = project_all(
        &mut projection,
        &[turn_start(), delta("so far"), tool_call("c1", "read")],
    );

    assert!(drafts.is_empty());
}

#[test]
fn names_the_tool_only_when_hints_are_on() {
    let mut without = TurnProjection::new(TurnProjectionOptions::default());
    let quiet_drafts = project_all(&mut without, &[turn_start(), tool_call("c1", "read")]);
    assert!(texts(&quiet_drafts).is_empty());

    let mut with = TurnProjection::new(hints());
    let loud = project_all(&mut with, &[turn_start(), tool_call("c1", "read")]);
    assert_eq!(texts(&loud), vec!["Running read…"]);
}

#[test]
fn names_the_tool_that_failed() {
    let mut projection = TurnProjection::new(hints());

    let drafts = project_all(
        &mut projection,
        &[
            turn_start(),
            tool_call("c1", "read"),
            tool_result("c1", false),
        ],
    );

    assert!(texts(&drafts).contains(&"read failed.".to_owned()));
}

#[test]
fn says_nothing_about_a_tool_that_succeeded() {
    let mut projection = TurnProjection::new(hints());

    let drafts = project_all(
        &mut projection,
        &[
            turn_start(),
            tool_call("c1", "read"),
            tool_result("c1", true),
        ],
    );

    assert!(!texts(&drafts).iter().any(|text| text.contains("failed")));
}

#[test]
fn falls_back_to_a_generic_name_for_a_result_with_no_call() {
    let mut projection = TurnProjection::new(hints());

    let drafts = project_all(
        &mut projection,
        &[turn_start(), tool_result("unknown", false)],
    );

    assert_eq!(texts(&drafts), vec!["a tool failed."]);
}

#[test]
fn says_nothing_about_a_failed_tool_when_hints_are_off() {
    let mut projection = TurnProjection::new(quiet());

    let drafts = project_all(
        &mut projection,
        &[
            turn_start(),
            tool_call("c1", "read"),
            tool_result("c1", false),
        ],
    );

    assert!(drafts.is_empty());
}

// Approvals

#[test]
fn announces_an_approval_request_whether_or_not_hints_are_on() {
    for options in [quiet(), hints()] {
        let mut projection = TurnProjection::new(options);
        let drafts = project_all(
            &mut projection,
            &[
                turn_start(),
                approval_request("c1", "exec", 1_700_000_000_000),
            ],
        );

        assert_eq!(kinds(&drafts), vec![OutboundKind::Notice]);
        assert!(drafts[0].text.contains("exec needs approval"));
        assert!(drafts[0].text.contains("denied automatically"));
    }
}

#[test]
fn carries_what_an_answer_needs_so_a_channel_can_offer_one() {
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = project_all(
        &mut projection,
        &[
            turn_start(),
            approval_request("call-9", "exec", 1_700_000_099_000),
        ],
    );

    let detail: ApprovalDraftDetail =
        serde_json::from_value(drafts[0].metadata[APPROVAL_METADATA_KEY].clone())
            .expect("the approval detail round-trips");
    assert_eq!(detail.call_id, "call-9");
    assert_eq!(detail.name, "exec");
    assert_eq!(detail.expires_at_ms, 1_700_000_099_000);
    assert_eq!(drafts[0].turn_id.as_deref(), Some(TURN));
}

#[test]
fn leaves_the_models_arguments_out_of_the_approval_detail() {
    // The one place a person is being asked to judge is the last place the
    // model's own words belong.
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = project_all(
        &mut projection,
        &[turn_start(), approval_request("c1", "exec", 0)],
    );

    let rendered = serde_json::to_string(&drafts[0].metadata).expect("metadata serialises");
    assert!(!rendered.contains("rm -rf"), "{rendered}");
    assert!(!rendered.contains("hunter2"), "{rendered}");
    assert!(!drafts[0].text.contains("rm -rf"));
}

#[test]
fn names_no_particular_place_to_answer_an_approval() {
    // It used to say "in the web UI", which was true only while nothing else
    // could, and would go on being shown by every channel that ignores the
    // detail.
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = project_all(
        &mut projection,
        &[turn_start(), approval_request("c1", "exec", 0)],
    );

    let text = drafts[0].text.to_lowercase();
    assert!(!text.contains("web ui"), "{text}");
    assert!(!text.contains("browser"), "{text}");
}

#[test]
fn leaves_metadata_off_a_draft_that_has_no_detail_to_carry() {
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = project_all(&mut projection, &[turn_start(), notice("heads up")]);

    assert!(drafts[0].metadata.is_empty());
}

// Stop reasons

#[test]
fn says_why_a_turn_stopped_after_handing_over_what_it_had_written() {
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = project_all(
        &mut projection,
        &[
            turn_start(),
            delta("partial"),
            turn_end(StopReason::Aborted),
        ],
    );

    assert_eq!(
        kinds(&drafts),
        vec![OutboundKind::Reply, OutboundKind::Notice]
    );
    assert_eq!(texts(&drafts), vec!["partial", "Stopped."]);
}

#[test]
fn reports_a_turn_that_produced_nothing_rather_than_staying_silent() {
    // A turn whose whole output was tool calls looks, from a chat app, exactly
    // like a bot that ignored the message.
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = project_all(
        &mut projection,
        &[turn_start(), turn_end(StopReason::Complete)],
    );

    assert_eq!(texts(&drafts), vec!["The turn finished without an answer."]);
}

#[test]
fn does_not_restate_an_error_the_hub_already_sent() {
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = project_all(
        &mut projection,
        &[
            turn_start(),
            error("the model is unreachable", Some(TURN)),
            turn_end(StopReason::Error),
        ],
    );

    assert_eq!(texts(&drafts), vec!["the model is unreachable"]);
    assert_eq!(kinds(&drafts), vec![OutboundKind::Error]);
}

#[test]
fn reports_the_iteration_cap_which_a_partial_answer_otherwise_hides() {
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = project_all(
        &mut projection,
        &[
            turn_start(),
            delta("half an answer"),
            turn_end(StopReason::MaxIterations),
        ],
    );

    assert_eq!(
        texts(&drafts),
        vec![
            "half an answer",
            "Stopped: the turn reached its tool-iteration limit."
        ]
    );
}

#[test]
fn reports_the_time_limit() {
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = project_all(
        &mut projection,
        &[
            turn_start(),
            delta("half"),
            turn_end(StopReason::WallTimeout),
        ],
    );

    assert_eq!(
        texts(&drafts).last().map(String::as_str),
        Some("Stopped: the turn reached its time limit.")
    );
}

// Everything else

#[test]
fn reports_a_queued_message_with_its_depth() {
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    assert_eq!(
        texts(&projection.project(&queued(1))),
        vec!["Queued behind 1 message."]
    );
    assert_eq!(
        texts(&projection.project(&queued(3))),
        vec!["Queued behind 3 messages."]
    );
}

#[test]
fn forwards_a_notice_as_written() {
    // So an injection badge is not lost on the way to a chat app.
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = projection.project(&notice("a tool result looked like an instruction"));

    assert_eq!(kinds(&drafts), vec![OutboundKind::Notice]);
    assert_eq!(
        texts(&drafts),
        vec!["a tool result looked like an instruction"]
    );
}

#[test]
fn an_error_outside_a_turn_carries_no_turn_id() {
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = projection.project(&error("the socket died", None));

    assert_eq!(drafts[0].turn_id, None);
}

#[test]
fn an_error_inside_a_turn_inherits_the_running_turn() {
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    projection.project(&turn_start());
    let drafts = projection.project(&error("the model is unreachable", None));

    assert_eq!(drafts[0].turn_id.as_deref(), Some(TURN));
}

#[test]
fn reduces_a_subagents_turn_to_its_two_ends() {
    let mut projection = TurnProjection::new(hints());

    let drafts = project_all(
        &mut projection,
        &[
            turn_start(),
            subagent("Researcher", nested_turn_start()),
            subagent("Researcher", nested_delta()),
            subagent("Researcher", nested_turn_end()),
        ],
    );

    assert_eq!(
        texts(&drafts),
        vec!["Asking Researcher…", "Researcher finished."]
    );
    // The nested deltas never reach the accumulator, so the caller's answer is
    // not interleaved with the subagent's working-out.
    assert_eq!(projection.answer(), "");
}

#[test]
fn names_a_subagent_by_its_id_when_it_has_no_label() {
    let mut projection = TurnProjection::new(hints());

    let drafts = project_all(
        &mut projection,
        &[turn_start(), subagent("", nested_turn_start())],
    );

    assert_eq!(texts(&drafts), vec!["Asking researcher…"]);
}

#[test]
fn says_nothing_about_a_subagent_when_hints_are_off() {
    let mut projection = TurnProjection::new(TurnProjectionOptions::default());

    let drafts = project_all(
        &mut projection,
        &[turn_start(), subagent("Researcher", nested_turn_start())],
    );

    assert!(drafts.is_empty());
}

#[test]
fn ignores_the_frames_a_browser_uses_to_reconcile_itself() {
    let mut projection = TurnProjection::new(hints());

    let drafts = project_all(
        &mut projection,
        &[
            turn_start(),
            context_usage(),
            session_status(true),
            reasoning("x"),
        ],
    );

    assert!(drafts.is_empty(), "{:?}", texts(&drafts));
}

#[test]
fn the_default_is_progress_on_and_hints_off() {
    // The pair a chat app can render without looking broken.
    let options = TurnProjectionOptions::default();

    assert!(options.send_progress);
    assert!(!options.send_tool_hints);
}

#[test]
fn the_session_a_status_names_is_the_one_it_belongs_to() {
    // A guard against the fixture drifting away from what the manager reads.
    let ServerMessage::SessionStatus(status) = session_status(true) else {
        panic!("session_status builds a session.status");
    };
    assert_eq!(status.event.session_key, SESSION);
    assert!(status.event.busy);
}
