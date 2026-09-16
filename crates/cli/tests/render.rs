//! `AgentEvent` to a terminal, asserted on the text rather than on escape
//! sequences: every case here builds its renderer with colour off, which is
//! what makes the expectations readable.

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

use std::sync::{Arc, Mutex};

use darkwire::i18n::Translations;
use darkwire::render::{
    DEFAULT_ARG_SUMMARY_CHARS, RenderTarget, TurnRenderer, TurnRendererOptions, clip, format_count,
    format_duration, format_rate, summarise_args,
};
use darkwire_agent::AgentEvent;
use darkwire_core::TurnStatsRecord;
use darkwire_protocol::{StopReason, TurnTiming, Usage};
use serde_json::{Value, json};

/// A target a test can read back, which is the whole reason [`RenderTarget`] is
/// a trait of its own rather than a writer.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<String>>);

impl Sink {
    fn text(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

impl RenderTarget for Sink {
    fn write(&mut self, text: &str) {
        self.0.lock().unwrap().push_str(text);
    }
}

/// The options every case here shares: a readable sink, no colour, English.
fn options(out: Sink) -> TurnRendererOptions {
    TurnRendererOptions {
        colors: Some(false),
        t: Translations::default(),
        ..TurnRendererOptions::new(Box::new(out))
    }
}

/// A renderer writing into a sink the caller keeps.
fn renderer() -> (TurnRenderer, Sink) {
    let sink = Sink::default();
    (TurnRenderer::new(options(sink.clone())), sink)
}

/// One event, addressed the way the loop addresses it.
fn event(value: Value) -> AgentEvent {
    serde_json::from_value(value).unwrap()
}

/// Every event through one renderer, finished, as text.
fn render(events: &[Value], build: impl FnOnce(&mut TurnRendererOptions)) -> String {
    let sink = Sink::default();
    let mut chosen = options(sink.clone());
    build(&mut chosen);
    let mut renderer = TurnRenderer::new(chosen);
    for value in events {
        renderer.handle(&event(value.clone()));
    }
    renderer.finish();
    sink.text()
}

/// The same, with the defaults.
fn plain(events: &[Value]) -> String {
    render(events, |_| {})
}

fn start() -> Value {
    json!({
        "type": "turn.start",
        "agentId": "default",
        "sessionKey": "cli:default",
        "turnId": "t1",
        "model": "qwen3",
        "provider": "ollama",
    })
}

// ---------------------------------------------------------------- echo

#[test]
fn echo_prints_the_operators_own_message_into_the_transcript() {
    // The editor clears the line on Return, so nothing else records what was
    // asked — the frame is not the transcript.
    let (mut renderer, sink) = renderer();
    renderer.echo("what is going on");
    assert!(sink.text().contains("› what is going on"));
}

#[test]
fn echo_leaves_one_blank_line_above_it() {
    // The gap between exchanges. It used to come from the prompt's own leading
    // newline; with the prompt gone it has to be written, and two messages in a
    // row would otherwise sit flush against the answer between them.
    let (mut renderer, sink) = renderer();
    renderer.note("an answer");
    renderer.echo("and then");

    assert_eq!(sink.text(), "an answer\n\n› and then\n");
}

// --------------------------------------------------------------- aside

#[test]
fn aside_starts_a_diagnostic_on_its_own_line() {
    // Logs reach the terminal through the same sink the answer does. Without
    // the break, a log line lands wherever the cursor is — measured mid-answer
    // as `- **Edit{"level":40,…} files**`.
    let (mut renderer, sink) = renderer();
    renderer.handle(&event(json!({
        "type": "assistant.delta",
        "turnId": "t",
        "text": "- **Edit",
    })));
    renderer.aside("{\"level\":40,\"msg\":\"mcp server unavailable\"}\n");

    assert_eq!(
        sink.text(),
        "- **Edit\n{\"level\":40,\"msg\":\"mcp server unavailable\"}\n"
    );
}

#[test]
fn aside_adds_no_break_when_a_line_has_just_ended() {
    let (mut renderer, sink) = renderer();
    renderer.note("a note");
    renderer.aside("a log\n");

    assert_eq!(sink.text(), "a note\na log\n");
}

// ---------------------------------------------------------------- clip

#[test]
fn clip_leaves_a_short_string_alone() {
    assert_eq!(clip("hello", 10), "hello");
}

#[test]
fn clip_counts_the_ellipsis_inside_the_budget() {
    assert_eq!(clip("abcdefghij", 5), "abcd…");
    assert_eq!(clip("abcdefghij", 5).chars().count(), 5);
}

#[test]
fn clip_collapses_whitespace_so_a_multi_line_value_stays_on_one_line() {
    assert_eq!(clip("a\n  b\tc ", 20), "a b c");
}

#[test]
fn clip_drops_an_astral_character_whole_rather_than_splitting_it() {
    // The budget is counted in UTF-16 units, so an emoji spends two of them —
    // and half a surrogate pair is a character no terminal can draw.
    assert_eq!(clip("ab🙂cd", 4), "ab…");
}

// ------------------------------------------------------- summarise_args

#[test]
fn summarise_args_renders_an_object_as_key_value_pairs() {
    assert_eq!(
        summarise_args(
            &json!({"path": "src", "recursive": true}),
            DEFAULT_ARG_SUMMARY_CHARS
        ),
        "path=\"src\" recursive=true"
    );
}

#[test]
fn summarise_args_passes_a_raw_string_through() {
    // That is a model that emitted invalid JSON, and the raw text is exactly
    // what is worth showing.
    assert_eq!(
        summarise_args(&json!("{\"path\": "), DEFAULT_ARG_SUMMARY_CHARS),
        "{\"path\":"
    );
}

#[test]
fn summarise_args_is_empty_for_no_arguments_at_all() {
    assert_eq!(
        summarise_args(&json!({}), DEFAULT_ARG_SUMMARY_CHARS),
        String::new()
    );
    // JSON has one spelling for absence where the original union had two, so
    // `null` is the empty case rather than the word.
    assert_eq!(
        summarise_args(&Value::Null, DEFAULT_ARG_SUMMARY_CHARS),
        String::new()
    );
}

#[test]
fn summarise_args_renders_an_array_by_stringifying_it() {
    assert_eq!(
        summarise_args(&json!(["git", "status"]), DEFAULT_ARG_SUMMARY_CHARS),
        "[\"git\",\"status\"]"
    );
}

#[test]
fn summarise_args_renders_a_null_value_inside_an_object() {
    assert_eq!(
        summarise_args(&json!({"a": null, "b": null}), DEFAULT_ARG_SUMMARY_CHARS),
        "a=null b=null"
    );
}

#[test]
fn summarise_args_clips_a_long_argument_list() {
    let summary = summarise_args(&json!({"content": "x".repeat(500)}), 40);
    assert_eq!(summary.chars().count(), 40);
    assert!(summary.ends_with('…'));
}

// ------------------------------------------------------- format_duration

#[test]
fn format_duration_reads_at_a_glance_at_every_scale() {
    assert_eq!(format_duration(12.0), "12ms");
    assert_eq!(format_duration(999.0), "999ms");
    assert_eq!(format_duration(1500.0), "1.5s");
    assert_eq!(format_duration(65_000.0), "1m 05s");
    // Without the hour branch a three-hour turn rendered as `187m 00s`, which
    // is a number a reader has to divide before it means anything.
    assert_eq!(format_duration(11_220_000.0), "3h 07m");
}

#[test]
fn format_count_switches_to_thousands_past_999() {
    assert_eq!(format_count(999), "999");
    assert_eq!(format_count(1204), "1.2k");
}

// -------------------------------------------------------- TurnRenderer

#[test]
fn streams_assistant_text_with_nothing_added_around_it() {
    let text = plain(&[
        start(),
        json!({"type": "assistant.delta", "turnId": "t1", "text": "Hel"}),
        json!({"type": "assistant.delta", "turnId": "t1", "text": "lo."}),
        json!({"type": "turn.end", "turnId": "t1", "stopReason": "complete", "iterations": 1}),
    ]);
    assert!(text.contains("Hello."));
    // No stop-reason line: a turn that completed normally says so by having
    // produced an answer, and announcing it on every turn is noise.
    assert!(!text.contains("stopped"));
}

#[test]
fn breaks_the_line_before_a_tool_card_when_text_did_not_end_on_one() {
    let text = plain(&[
        start(),
        json!({"type": "assistant.delta", "turnId": "t1", "text": "Let me look"}),
        json!({
            "type": "tool.call", "turnId": "t1", "callId": "c1",
            "name": "list_dir", "args": {}, "risk": "safe",
        }),
    ]);
    assert!(text.contains("Let me look\n"));
    assert!(text.contains("\n⚙ list_dir"));
}

#[test]
fn does_not_add_a_second_newline_when_the_text_already_ended_on_one() {
    let text = plain(&[
        start(),
        json!({"type": "assistant.delta", "turnId": "t1", "text": "Done.\n"}),
        json!({
            "type": "tool.call", "turnId": "t1", "callId": "c1",
            "name": "exec", "args": {}, "risk": "exec",
        }),
    ]);
    assert!(!text.contains("\n\n"));
}

#[test]
fn labels_a_tool_call_with_its_arguments() {
    let text = plain(&[
        start(),
        json!({
            "type": "tool.call", "turnId": "t1", "callId": "c1",
            "name": "read_file", "args": {"path": "README.md"}, "risk": "safe",
        }),
    ]);
    assert!(text.contains("⚙ read_file path=\"README.md\""));
}

#[test]
fn previews_a_tool_result_and_says_how_much_it_is_hiding() {
    let text = render(
        &[
            start(),
            json!({
                "type": "tool.result", "turnId": "t1", "callId": "c1", "ok": true,
                "content": "a\nb\nc\nd", "truncated": false, "durationMs": 120,
            }),
        ],
        |built| built.tool_result_lines = 2,
    );
    assert!(text.contains("✓ 120ms"));
    assert!(text.contains("    a\n"));
    assert!(text.contains("    b\n"));
    assert!(!text.contains("    c\n"));
    assert!(text.contains("… 2 more lines"));
}

#[test]
fn marks_a_failed_call_and_reports_truncation() {
    let text = plain(&[
        start(),
        json!({
            "type": "tool.result", "turnId": "t1", "callId": "c1", "ok": false,
            "content": "ENOENT", "truncated": true, "durationMs": 4,
        }),
    ]);
    assert!(text.contains("✗ 4ms, truncated"));
}

#[test]
fn names_the_running_tool_in_a_heartbeat() {
    let text = plain(&[
        start(),
        json!({
            "type": "tool.call", "turnId": "t1", "callId": "c1",
            "name": "exec", "args": {}, "risk": "exec",
        }),
        json!({"type": "tool.progress", "turnId": "t1", "callId": "c1", "elapsedMs": 15_000}),
    ]);
    assert!(text.contains("… exec 15.0s"));
}

#[test]
fn falls_back_to_a_generic_label_for_a_heartbeat_it_never_saw_the_call_for() {
    let text = plain(&[
        start(),
        json!({"type": "tool.progress", "turnId": "t1", "callId": "unknown", "elapsedMs": 1000}),
    ]);
    assert!(text.contains("… tool 1.0s"));
}

#[test]
fn breaks_between_the_reasoning_and_the_answer_and_labels_neither() {
    // The reasoning used to carry a `┄ thinking` header. It read as a label on
    // something that does not need one: reasoning arrives before the answer,
    // ends at a line break, and is the only dim run in a turn.
    let text = plain(&[
        start(),
        json!({"type": "reasoning.delta", "turnId": "t1", "text": "weighing options"}),
        json!({"type": "assistant.delta", "turnId": "t1", "text": "Yes."}),
    ]);
    assert!(text.contains("weighing options\nYes."));
    assert!(!text.contains("thinking"));
}

#[test]
fn hides_reasoning_entirely_when_asked_to() {
    let text = render(
        &[
            start(),
            json!({"type": "reasoning.delta", "turnId": "t1", "text": "secret"}),
        ],
        |built| built.show_reasoning = false,
    );
    assert!(!text.contains("secret"));
    assert!(!text.contains("thinking"));
}

#[test]
fn shows_a_notice_without_letting_it_look_like_the_answer() {
    let text = plain(&[
        start(),
        json!({
            "type": "notice", "kind": "prompt_injection", "turnId": "t1",
            "message": "Tool output contained instruction-like text.",
        }),
    ]);
    assert!(text.contains("⚠ Tool output contained instruction-like text."));
}

#[test]
fn prints_an_error_with_its_code_and_whether_retrying_is_worth_it() {
    let text = plain(&[
        start(),
        json!({
            "type": "error", "code": "provider_error", "retryable": true, "turnId": "t1",
            "message": "The provider ended the stream without a result.",
        }),
    ]);
    assert!(text.contains("✖ The provider ended the stream without a result."));
    assert!(text.contains("provider_error · retryable"));
}

#[test]
fn explains_a_turn_that_hit_a_cap() {
    let text = plain(&[
        start(),
        json!({
            "type": "turn.end", "turnId": "t1",
            "stopReason": "max_iterations", "iterations": 40,
        }),
    ]);
    assert!(text.contains("stopped at the tool-iteration cap"));
    assert!(text.contains("40 steps"));
}

#[test]
fn reports_usage_when_the_provider_sent_any() {
    let text = plain(&[
        start(),
        json!({
            "type": "turn.end", "turnId": "t1", "stopReason": "complete", "iterations": 1,
            "usage": {
                "promptTokens": 1204, "completionTokens": 88, "totalTokens": 1292,
                "cachedTokens": 1000, "reasoningTokens": 40,
            },
        }),
    ]);
    assert!(text.contains("1 step"));
    assert!(text.contains("1.2k in / 88 out / 1.0k cached / 40 reasoning"));
}

#[test]
fn omits_the_summary_line_when_usage_is_switched_off() {
    let text = render(
        &[
            start(),
            json!({
                "type": "turn.end", "turnId": "t1",
                "stopReason": "complete", "iterations": 2,
            }),
        ],
        |built| built.show_stats = false,
    );
    assert!(!text.contains("steps"));
}

#[test]
fn emits_ansi_only_when_colour_is_enabled() {
    let call = json!({
        "type": "tool.call", "turnId": "t1", "callId": "c1",
        "name": "exec", "args": {}, "risk": "exec",
    });

    let plain_sink = Sink::default();
    TurnRenderer::new(options(plain_sink.clone())).handle(&event(call.clone()));

    let coloured_sink = Sink::default();
    TurnRenderer::new(TurnRendererOptions {
        colors: Some(true),
        ..options(coloured_sink.clone())
    })
    .handle(&event(call));

    assert!(!plain_sink.text().contains('\u{1b}'));
    assert!(coloured_sink.text().contains('\u{1b}'));
}

#[test]
fn writes_its_own_notes_in_the_same_line_discipline() {
    let (mut renderer, sink) = renderer();
    renderer.handle(&event(
        json!({"type": "assistant.delta", "turnId": "t1", "text": "mid-line"}),
    ));
    renderer.note("a note");
    renderer.warn("a warning");
    assert_eq!(sink.text(), "mid-line\na note\n⚠ a warning\n");
}

#[test]
fn says_a_tool_is_waiting_for_an_approval_it_cannot_answer() {
    // `darkwire chat` installs no gate, so reaching here means the CLI is
    // watching a turn some other surface is driving. Saying so beats a gap.
    let text = plain(&[
        start(),
        json!({
            "type": "tool.approvalRequest", "turnId": "t1", "callId": "c1",
            "name": "exec", "args": {}, "risk": "exec", "expiresAtMs": 1,
        }),
    ]);
    assert!(text.contains("⧗ exec is waiting for approval elsewhere"));
}

/// One recorded turn, with only the fields a stats line reads set apart.
fn stats_row(
    model: &str,
    started_at_ms: i64,
    ended_at_ms: i64,
    iterations: i64,
) -> TurnStatsRecord {
    TurnStatsRecord {
        turn_id: "t1".to_owned(),
        session_key: "cli:default".to_owned(),
        agent_id: "default".to_owned(),
        workspace_id: "default".to_owned(),
        provider: "ollama".to_owned(),
        model: model.to_owned(),
        started_at_ms,
        ended_at_ms,
        iterations,
        stop_reason: StopReason::Complete,
        usage: rate_usage(),
        generation_ms: None,
        generation_tokens: None,
        first_token_ms: None,
        error: None,
    }
}

#[test]
fn reports_what_past_turns_cost_one_line_each() {
    let (mut renderer, sink) = renderer();
    renderer.stats(&[stats_row("qwen3", 1_000, 2_000, 2)]);

    let text = sink.text();
    assert!(text.contains("qwen3 · 2 steps · 100 in / 250 out · 1.0s · 250.0 tok/s"));
}

#[test]
fn names_a_turn_recorded_before_the_model_was_captured() {
    let (mut renderer, sink) = renderer();
    renderer.stats(&[stats_row("", 0, 500, 1)]);
    assert!(sink.text().contains("unknown model"));
}

// ---------------------------------------------------------- format_rate

fn rate_usage() -> Usage {
    Usage {
        prompt_tokens: 100,
        completion_tokens: 250,
        total_tokens: 350,
        cached_tokens: None,
        reasoning_tokens: None,
    }
}

#[test]
fn format_rate_reports_completion_tokens_per_second() {
    let timing = TurnTiming {
        elapsed_ms: Some(1000.0),
        ..TurnTiming::default()
    };
    assert_eq!(
        format_rate(&rate_usage(), &timing),
        Some("250.0 tok/s".to_owned())
    );
}

#[test]
fn format_rate_divides_by_generation_time_when_the_provider_measured_it() {
    // The same turn, whose wall clock is ten seconds because the model spent
    // nine of them loading its weights. The wall-clock divisor called this a
    // 25 tok/s model; it is a 250 tok/s model that was not running for most of
    // the turn.
    let timing = TurnTiming {
        generation_ms: Some(1000.0),
        generation_tokens: Some(250.0),
        elapsed_ms: Some(10_000.0),
    };
    assert_eq!(
        format_rate(&rate_usage(), &timing),
        Some("250.0 tok/s".to_owned())
    );
}

#[test]
fn format_rate_divides_only_the_tokens_that_were_timed() {
    // A turn that also made a bare tool call, which some providers send as a
    // single frame — charged for, measured at zero, and so excluded from both
    // sides.
    let timing = TurnTiming {
        generation_ms: Some(1000.0),
        generation_tokens: Some(100.0),
        elapsed_ms: Some(10_000.0),
    };
    assert_eq!(
        format_rate(&rate_usage(), &timing),
        Some("100.0 tok/s".to_owned())
    );
}

#[test]
fn format_rate_falls_back_to_the_wall_clock_for_a_turn_recorded_before_that() {
    // Every row written by an older build, and any reply that arrived in a
    // single frame. Blanking those would be a regression dressed as accuracy.
    let zeroed = TurnTiming {
        generation_ms: Some(0.0),
        generation_tokens: Some(0.0),
        elapsed_ms: Some(1000.0),
    };
    assert_eq!(
        format_rate(&rate_usage(), &zeroed),
        Some("250.0 tok/s".to_owned())
    );

    let absent = TurnTiming {
        elapsed_ms: Some(1000.0),
        ..TurnTiming::default()
    };
    assert_eq!(
        format_rate(&rate_usage(), &absent),
        Some("250.0 tok/s".to_owned())
    );

    // Half a measurement is not a measurement.
    let half = TurnTiming {
        generation_ms: Some(1000.0),
        elapsed_ms: Some(1000.0),
        ..TurnTiming::default()
    };
    assert_eq!(
        format_rate(&rate_usage(), &half),
        Some("250.0 tok/s".to_owned())
    );
}

#[test]
fn format_rate_reports_nothing_rather_than_dividing_by_an_unmeasured_turn() {
    // A turn that finished inside one millisecond is common on a scripted
    // provider and on a fast local model. A rate derived from that zero is a
    // number that looks measured and is not.
    let zero = TurnTiming {
        elapsed_ms: Some(0.0),
        ..TurnTiming::default()
    };
    assert_eq!(format_rate(&rate_usage(), &zero), None);
    assert_eq!(format_rate(&rate_usage(), &TurnTiming::default()), None);
}

#[test]
fn format_rate_reports_nothing_for_a_turn_that_produced_no_tokens() {
    let usage = Usage {
        prompt_tokens: 10,
        completion_tokens: 0,
        total_tokens: 10,
        cached_tokens: None,
        reasoning_tokens: None,
    };
    let timing = TurnTiming {
        elapsed_ms: Some(500.0),
        ..TurnTiming::default()
    };
    assert_eq!(format_rate(&usage, &timing), None);
}

// ----------------------------------------------------------- subagents

/// One nested event, addressed as the loop addresses it.
fn nested(inner: &Value, depth: u64) -> Value {
    json!({
        "type": "subagent.event",
        "turnId": "t1",
        "parentSessionKey": "cli:default",
        "parentCallId": "c1",
        "agentId": "researcher",
        "label": "Researcher",
        "sessionKey": "sub-1",
        "depth": depth,
        "event": inner,
    })
}

fn child_start() -> Value {
    json!({
        "type": "turn.start",
        "agentId": "researcher",
        "sessionKey": "sub-1",
        "turnId": "t2",
        "model": "qwen3",
        "provider": "ollama",
    })
}

#[test]
fn opens_and_closes_a_delegation_with_a_rule_of_its_own() {
    let text = plain(&[
        start(),
        json!({
            "type": "tool.call", "turnId": "t1", "callId": "c1",
            "name": "ask_researcher", "args": {}, "risk": "safe",
        }),
        nested(&child_start(), 1),
        nested(
            &json!({"type": "assistant.delta", "turnId": "t2", "text": "Found it."}),
            1,
        ),
        nested(
            &json!({"type": "turn.end", "turnId": "t2", "stopReason": "complete", "iterations": 1}),
            1,
        ),
    ]);

    assert!(text.contains("┄ asking Researcher"));
    assert!(text.contains("┄ Researcher finished"));
}

#[test]
fn indents_the_subagents_work_under_the_call_that_started_it() {
    let text = plain(&[
        start(),
        nested(&child_start(), 1),
        nested(
            &json!({
                "type": "tool.call", "turnId": "t2", "callId": "n1",
                "name": "list_dir", "args": {"path": "src"}, "risk": "safe",
            }),
            1,
        ),
        nested(
            &json!({
                "type": "tool.result", "turnId": "t2", "callId": "n1", "ok": true,
                "content": "", "truncated": false, "durationMs": 12,
            }),
            1,
        ),
    ]);

    assert!(text.contains("  ⚙ list_dir"));
    // The caller's own tool line has no indent, so the two are distinguishable.
    assert!(!text.contains("\n⚙ list_dir"));
}

#[test]
fn indents_every_line_of_a_subagents_answer_not_only_the_first() {
    let text = plain(&[
        start(),
        nested(&child_start(), 1),
        // Two chunks, the first ending mid-line — which is how a stream
        // arrives.
        nested(
            &json!({"type": "assistant.delta", "turnId": "t2", "text": "one\ntwo"}),
            1,
        ),
        nested(
            &json!({"type": "assistant.delta", "turnId": "t2", "text": "\nthree\n"}),
            1,
        ),
    ]);

    assert!(text.contains("  one\n  two\n  three\n"));
}

#[test]
fn goes_one_level_further_in_for_a_subagent_of_a_subagent() {
    let text = plain(&[
        start(),
        nested(
            &json!({
                "type": "tool.call", "turnId": "t3", "callId": "g1",
                "name": "read_file", "args": {}, "risk": "safe",
            }),
            2,
        ),
    ]);

    assert!(text.contains("    ⚙ read_file"));
}

#[test]
fn does_not_let_a_subagents_turn_clear_the_callers_tool_labels() {
    let text = plain(&[
        start(),
        json!({
            "type": "tool.call", "turnId": "t1", "callId": "c1",
            "name": "ask_researcher", "args": {}, "risk": "safe",
        }),
        nested(&child_start(), 1),
        nested(
            &json!({"type": "turn.end", "turnId": "t2", "stopReason": "complete", "iterations": 1}),
            1,
        ),
        // The caller's own progress event still knows what `c1` was.
        json!({"type": "tool.progress", "turnId": "t1", "callId": "c1", "elapsedMs": 15_000}),
    ]);

    assert!(text.contains("… ask_researcher"));
    assert!(!text.contains("… tool"));
}

#[test]
fn keeps_a_parent_and_a_subagent_call_with_the_same_id_apart() {
    let text = render(
        &[
            start(),
            json!({
                "type": "tool.call", "turnId": "t1", "callId": "x",
                "name": "ask_researcher", "args": {}, "risk": "safe",
            }),
            nested(&child_start(), 1),
            // The subagent's model mints the same call id — legal, and its
            // result must not delete the label the caller is still using.
            nested(
                &json!({
                    "type": "tool.call", "turnId": "t2", "callId": "x",
                    "name": "echo", "args": {}, "risk": "safe",
                }),
                1,
            ),
            nested(
                &json!({
                    "type": "tool.result", "turnId": "t2", "callId": "x", "ok": true,
                    "content": "", "truncated": false, "durationMs": 1,
                }),
                1,
            ),
            json!({"type": "tool.progress", "turnId": "t1", "callId": "x", "elapsedMs": 15_000}),
        ],
        |built| built.tool_result_lines = 0,
    );

    assert!(text.contains("… ask_researcher"));
}

#[test]
fn a_context_usage_event_draws_nothing() {
    // The terminal keeps the same figure in its header and refreshes it at the
    // same turn boundaries, so a line here would be the number twice.
    let text = plain(&[
        start(),
        json!({
            "type": "context.usage", "sessionKey": "cli:default",
            "estimatedTokens": 100, "contextWindowTokens": 8192,
        }),
    ]);
    assert_eq!(text, String::new());
}
