//! The provider's count of a request, stored beside the estimate of it and fed
//! back into the meter and the history budget.
//!
//! The harness's first request estimates at about 600 tokens, so a reported
//! 1,000 is a believable count, 300 a believable one that is suspiciously low,
//! and 100 one that describes some other request.

use crate::common;

use common::harness::{FakeTool, Harness, Setup, events_of};
use darkwire_agent::testkit::{ScriptedTurn, raw_tool_call, tool_call};
use darkwire_agent::{MeasureContext, PromptPreview, measure_context};
use darkwire_protocol::{Calibration, ChatMessage, Usage};
use darkwire_providers::FinishReason;
use serde_json::json;

fn usage(prompt: u64) -> Usage {
    Usage {
        prompt_tokens: prompt,
        completion_tokens: 5,
        total_tokens: prompt + 5,
        cached_tokens: None,
        reasoning_tokens: None,
    }
}

fn answered(usage: Usage) -> Harness {
    Harness::build(Setup {
        turns: vec![ScriptedTurn {
            usage: Some(usage),
            ..ScriptedTurn::text("hi")
        }],
        ..Setup::default()
    })
}

async fn sample_of(harness: &Harness) -> Option<Calibration> {
    harness.say("web:1", "hello").await.1.expect("a turn");
    match &harness.stored("web:1")[1] {
        ChatMessage::Assistant(assistant) => assistant.stats.as_ref()?.calibration.clone(),
        other => panic!("not an answer: {other:?}"),
    }
}

#[tokio::test]
async fn a_counted_request_is_stored_beside_its_estimate() {
    let sample = sample_of(&answered(usage(1_000))).await.expect("a sample");
    assert_eq!(sample.reported_tokens, 1_000);
    assert!(
        (300..1_000).contains(&sample.estimated_tokens),
        "{sample:?}"
    );
    // The id the request named, which is what the meter compares against.
    assert_eq!(sample.model, "test-model");
}

#[tokio::test]
async fn a_count_that_cannot_be_believed_is_not_stored() {
    // Nothing reported: a missing field decodes as zero.
    assert_eq!(sample_of(&answered(usage(0))).await, None);
    // Far below the estimate: a cache-miss count, or a truncated prompt.
    assert_eq!(sample_of(&answered(usage(100))).await, None);
    // More cached than counted is a server describing something else.
    let cached = Usage {
        cached_tokens: Some(2_000),
        ..usage(1_000)
    };
    assert_eq!(sample_of(&answered(cached)).await, None);
}

#[tokio::test]
async fn a_request_the_ladder_rewrote_is_not_a_sample() {
    // Its count describes the smaller request the provider accepted, not the
    // one the loop estimated.
    let harness = Harness::build(Setup {
        turns: vec![ScriptedTurn {
            usage: Some(usage(1_000)),
            degraded: true,
            ..ScriptedTurn::text("hi")
        }],
        ..Setup::default()
    });
    assert_eq!(sample_of(&harness).await, None);
}

#[tokio::test]
async fn a_call_that_wrote_nothing_adds_its_count_to_the_next_rows() {
    // The step's usage sums both calls, so its calibration has to sum both
    // sides too, or the ratio would set two requests' count against one
    // request's estimate.
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn {
                finish_reason: Some(FinishReason::Length),
                usage: Some(usage(1_000)),
                ..ScriptedTurn::calls(vec![raw_tool_call("c1", "write", r#"{"path": "#)])
            },
            ScriptedTurn {
                usage: Some(usage(1_100)),
                ..ScriptedTurn::text("done")
            },
        ],
        tools: vec![FakeTool::reading("write", "wrote")],
        ..Setup::default()
    });
    let sample = sample_of(&harness).await.expect("a sample");
    assert_eq!(sample.reported_tokens, 2_100);
    let first = harness.provider.requests().len();
    assert_eq!(first, 2, "two requests behind one row");
    // Two estimates of about 600 each, not one.
    assert!(sample.estimated_tokens > 1_000, "{sample:?}");
}

#[tokio::test]
async fn a_count_well_below_the_estimate_is_kept_and_said_out_loud() {
    let harness = answered(usage(300));
    let (events, result) = harness.say("web:1", "hello").await;
    result.expect("a turn");

    let notices = events_of(&events, "notice");
    let undercount = notices
        .iter()
        .find(|notice| {
            notice["message"]
                .as_str()
                .is_some_and(|text| text.contains("num_ctx"))
        })
        .expect("a notice naming the likely cause");
    assert_eq!(undercount["kind"], json!("truncated_history"));
}

#[tokio::test]
async fn the_meter_reads_in_the_providers_tokens_after_a_request() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn {
                usage: Some(usage(1_000)),
                ..ScriptedTurn::calls(vec![tool_call("c1", "read", &json!({"path": "a.md"}))])
            },
            ScriptedTurn {
                usage: Some(usage(1_100)),
                ..ScriptedTurn::text("It says hello.")
            },
        ],
        tools: vec![FakeTool::reading("read", "file contents")],
        ..Setup::default()
    });
    let (events, result) = harness.say("web:1", "read it").await;
    result.expect("a turn");

    let usage = &events_of(&events, "context.usage")[0];
    assert_eq!(usage["measured"], json!(true));
    // Scaled up from the ~600 the estimate says, and the sections still add up.
    let total = usage["estimatedTokens"].as_u64().expect("a total");
    assert!(total > 1_000, "{usage}");
    let sections: f64 = usage["breakdown"]
        .as_object()
        .expect("a breakdown")
        .values()
        .filter_map(serde_json::Value::as_f64)
        .sum();
    #[allow(clippy::cast_precision_loss, reason = "a token count far below 2^53")]
    let total = total as f64;
    assert!((sections - total).abs() < f64::EPSILON, "{usage}");
}

#[tokio::test]
async fn a_report_scales_by_the_newest_count_on_the_same_model() {
    let harness = answered(usage(1_000));
    let sample = sample_of(&harness).await.expect("a sample");
    let prompt = PromptPreview {
        static_prompt: "You are a helpful agent working in a workspace.".to_owned(),
        runtime_block: String::new(),
    };
    let report = |model: &str| {
        measure_context(&MeasureContext {
            store: &harness.store,
            tools: &[],
            session_key: "web:1",
            prompt: &prompt,
            context_window_tokens: 65_536,
            model,
            vision: true,
        })
        .expect("a report")
    };

    let measured = report("test-model");
    // Another model's tokenizer says nothing about this one.
    let estimated = report("another-model");
    assert!(measured.measured);
    assert!(!estimated.measured);

    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "token counts far below 2^53"
    )]
    let scaled = (estimated.estimated_tokens as f64 * sample.ratio()).round() as usize;
    assert_eq!(measured.estimated_tokens, scaled);
    assert_eq!(measured.breakdown.total(), measured.estimated_tokens);
}
