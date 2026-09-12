//! The outbound policy: what posts, what edits, what is dropped.

use std::sync::Arc;

use ghostai_channels::telegram::api::{
    BotApi, HttpClient, InlineKeyboardButton, InlineKeyboardMarkup,
};
use ghostai_channels::telegram::chats::{ChatState, RenderPrefs};
use ghostai_channels::telegram::render::{RenderOutcome, RenderRequest, TelegramRenderer};
use ghostai_core::clock::Clock;
use ghostai_core::message_bus::OutboundKind;
use ghostai_core::testkit::ManualClock;
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::fake_bot_api::{CannedAnswer, FakeBotApi};

const NOW: i64 = 1_700_000_000_000;
const EDIT_INTERVAL_MS: i64 = 2000;
const CHAT: i64 = 4471;

struct Harness {
    renderer: TelegramRenderer,
    api: Arc<FakeBotApi>,
    clock: Arc<ManualClock>,
}

fn harness() -> Harness {
    let api = FakeBotApi::new();
    let clock = Arc::new(ManualClock::at(NOW));
    let bot = Arc::new(BotApi::new(
        "12345:secret",
        "http://telegram.test",
        Arc::clone(&api) as Arc<dyn HttpClient>,
    ));
    Harness {
        renderer: TelegramRenderer::new(
            bot,
            Arc::clone(&clock) as Arc<dyn Clock>,
            EDIT_INTERVAL_MS,
            "telegram",
        ),
        api,
        clock,
    }
}

fn chat() -> ChatState {
    ChatState {
        session_key: "telegram:4471".to_owned(),
        live_message_id: None,
        live_turn_id: None,
        last_edit_ms: 0,
        prefs: RenderPrefs::default(),
    }
}

fn request(text: &str, kind: OutboundKind) -> RenderRequest {
    RenderRequest {
        chat_id: CHAT,
        text: text.to_owned(),
        kind,
        turn_id: None,
        keyboard: None,
    }
}

fn keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup {
        inline_keyboard: vec![vec![InlineKeyboardButton {
            text: "Once".to_owned(),
            callback_data: "t1".to_owned(),
        }]],
    }
}

fn live() -> CancellationToken {
    CancellationToken::new()
}

async fn render(harness: &Harness, request: &RenderRequest, chat: &ChatState) -> RenderOutcome {
    harness.renderer.render(request, chat, &live()).await
}

// Posting

#[tokio::test]
async fn a_notice_posts_a_fresh_message() {
    let harness = harness();

    let outcome = render(
        &harness,
        &request("heads up", OutboundKind::Notice),
        &chat(),
    )
    .await;

    assert_eq!(harness.api.count("sendMessage"), 1);
    assert_eq!(harness.api.count("editMessageText"), 0);
    assert!(outcome.posted.is_some());
    // A notice never claims the turn's message: an answer must not overwrite the
    // warning before it.
    assert_eq!(outcome.live_message_id, None);
}

#[tokio::test]
async fn text_is_escaped_for_markdown_when_the_chat_wants_it() {
    let harness = harness();

    render(&harness, &request("Done.", OutboundKind::Reply), &chat()).await;

    let body = &harness.api.bodies("sendMessage")[0];
    assert_eq!(body["text"], json!(r"Done\."));
    assert_eq!(body["parse_mode"], json!("MarkdownV2"));
}

#[tokio::test]
async fn plain_text_is_sent_as_it_stands() {
    let harness = harness();
    let mut chat = chat();
    chat.prefs.markdown = false;

    render(&harness, &request("Done.", OutboundKind::Reply), &chat).await;

    let body = &harness.api.bodies("sendMessage")[0];
    assert_eq!(body["text"], json!("Done."));
    assert!(!body.contains_key("parse_mode"));
}

#[tokio::test]
async fn an_over_long_answer_is_split_and_only_the_last_piece_carries_the_buttons() {
    // A keyboard repeated once per chunk of a long card is four keyboards.
    let harness = harness();
    let mut request = request(&"x".repeat(9000), OutboundKind::Notice);
    request.keyboard = Some(keyboard());

    render(&harness, &request, &chat()).await;

    let bodies = harness.api.bodies("sendMessage");
    assert!(bodies.len() > 1, "{} pieces", bodies.len());
    assert!(!bodies[0].contains_key("reply_markup"));
    assert!(bodies.last().expect("a piece").contains_key("reply_markup"));
}

#[tokio::test]
async fn the_first_message_id_is_reported_for_a_card_the_caller_will_edit() {
    let harness = harness();
    harness.api.reply(
        "sendMessage",
        CannedAnswer::ok(&json!({
            "message_id": 555,
            "chat": { "id": CHAT, "type": "private" },
        })),
    );

    let outcome = render(
        &harness,
        &request("pick one", OutboundKind::Notice),
        &chat(),
    )
    .await;

    assert_eq!(outcome.posted, Some(555));
}

// progress and the turn's own message

#[tokio::test]
async fn progress_is_withheld_from_a_chat_that_turned_it_off() {
    let harness = harness();
    let mut chat = chat();
    chat.prefs.progress = false;

    let outcome = render(&harness, &request("so far", OutboundKind::Progress), &chat).await;

    assert_eq!(harness.api.count("sendMessage"), 0);
    assert_eq!(outcome.posted, None);
}

#[tokio::test]
async fn the_first_progress_posts_and_claims_the_message_the_turn_fills_in() {
    let harness = harness();
    harness.api.reply(
        "sendMessage",
        CannedAnswer::ok(&json!({ "message_id": 77, "chat": { "id": CHAT, "type": "private" } })),
    );
    let mut request = request("so far", OutboundKind::Progress);
    request.turn_id = Some("turn-1".to_owned());

    let outcome = render(&harness, &request, &chat()).await;

    assert_eq!(outcome.posted, Some(77));
    assert_eq!(outcome.live_message_id, Some(77));
    assert_eq!(outcome.live_turn_id.as_deref(), Some("turn-1"));
    assert_eq!(outcome.last_edit_ms, NOW);
}

#[tokio::test]
async fn the_rest_of_the_turn_edits_that_message_rather_than_posting_again() {
    // The answer arrives by filling in rather than by being repeated twice.
    let harness = harness();
    let mut chat = chat();
    chat.live_message_id = Some(77);
    chat.live_turn_id = Some("turn-1".to_owned());
    chat.last_edit_ms = NOW - EDIT_INTERVAL_MS;
    let mut request = request("so far, and more", OutboundKind::Progress);
    request.turn_id = Some("turn-1".to_owned());

    render(&harness, &request, &chat).await;

    assert_eq!(harness.api.count("sendMessage"), 0);
    assert_eq!(
        harness.api.bodies("editMessageText")[0]["message_id"],
        json!(77)
    );
}

#[tokio::test]
async fn an_intermediate_progress_inside_the_debounce_is_skipped_rather_than_queued() {
    // The next one carries everything it did, and nothing here may sleep: every
    // chat shares one delivery chain.
    let harness = harness();
    let mut chat = chat();
    chat.live_message_id = Some(77);
    chat.live_turn_id = Some("turn-1".to_owned());
    chat.last_edit_ms = NOW;
    let mut request = request("so far", OutboundKind::Progress);
    request.turn_id = Some("turn-1".to_owned());

    render(&harness, &request, &chat).await;

    assert_eq!(harness.api.count("editMessageText"), 0);
}

#[tokio::test]
async fn a_reply_always_lands_however_recent_the_last_edit_was() {
    let harness = harness();
    let mut chat = chat();
    chat.live_message_id = Some(77);
    chat.live_turn_id = Some("turn-1".to_owned());
    chat.last_edit_ms = NOW;
    let mut request = request("the whole answer", OutboundKind::Reply);
    request.turn_id = Some("turn-1".to_owned());

    let outcome = render(&harness, &request, &chat).await;

    assert_eq!(harness.api.count("editMessageText"), 1);
    // And the turn is over, so the message stops being live.
    assert_eq!(outcome.live_message_id, None);
    assert_eq!(outcome.live_turn_id, None);
}

#[tokio::test]
async fn a_message_from_another_turn_posts_rather_than_rewriting_this_one() {
    let harness = harness();
    let mut chat = chat();
    chat.live_message_id = Some(77);
    chat.live_turn_id = Some("turn-1".to_owned());
    let mut request = request("a later answer", OutboundKind::Reply);
    request.turn_id = Some("turn-2".to_owned());

    render(&harness, &request, &chat).await;

    assert_eq!(harness.api.count("sendMessage"), 1);
    assert_eq!(harness.api.count("editMessageText"), 0);
}

#[tokio::test]
async fn an_answer_that_outgrew_one_message_stops_collecting_edits_into_it() {
    let harness = harness();
    let mut chat = chat();
    chat.live_message_id = Some(77);
    chat.live_turn_id = Some("turn-1".to_owned());
    let mut request = request(&"x".repeat(9000), OutboundKind::Reply);
    request.turn_id = Some("turn-1".to_owned());

    let outcome = render(&harness, &request, &chat).await;

    assert!(harness.api.count("sendMessage") > 1);
    assert_eq!(outcome.live_message_id, None);
}

#[tokio::test]
async fn a_turn_that_claims_nothing_leaves_the_live_message_where_it_was() {
    // A `notice` mid-turn posts and must not steal the message the answer is
    // being written into.
    let harness = harness();
    let mut chat = chat();
    chat.live_message_id = Some(77);
    chat.live_turn_id = Some("turn-1".to_owned());

    let outcome = render(&harness, &request("heads up", OutboundKind::Notice), &chat).await;

    assert_eq!(harness.api.count("sendMessage"), 1);
    assert_eq!(outcome.live_message_id, None);
}

// Failures

#[tokio::test]
async fn a_message_telegram_will_not_parse_is_sent_again_as_plain_text() {
    // That fallback is what lets the formatter stay small: a gap in it costs
    // formatting, not the answer.
    let harness = harness();
    harness
        .api
        .fail("sendMessage", 400, "Bad Request: can't parse entities");
    harness.api.reply(
        "sendMessage",
        CannedAnswer::ok(&json!({ "message_id": 9, "chat": { "id": CHAT, "type": "private" } })),
    );

    let outcome = render(&harness, &request("Done.", OutboundKind::Reply), &chat()).await;

    let bodies = harness.api.bodies("sendMessage");
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0]["parse_mode"], json!("MarkdownV2"));
    // Deliberately without a parse mode, and with the escaping undone.
    assert!(!bodies[1].contains_key("parse_mode"));
    assert_eq!(bodies[1]["text"], json!("Done."));
    assert_eq!(outcome.posted, Some(9));
}

#[tokio::test]
async fn a_retry_that_also_fails_is_dropped_rather_than_raised() {
    let harness = harness();
    harness.api.fail("sendMessage", 400, "Bad Request");

    let outcome = render(&harness, &request("Done.", OutboundKind::Reply), &chat()).await;

    assert_eq!(harness.api.count("sendMessage"), 2);
    assert_eq!(outcome.posted, None);
}

#[tokio::test]
async fn a_rate_limit_longer_than_a_second_is_dropped_rather_than_waited_out() {
    // The sleep would sit on the chain every other conversation is queued
    // behind.
    let harness = harness();
    harness.api.rate_limit("sendMessage", 30);

    let outcome = render(
        &harness,
        &request("the answer", OutboundKind::Reply),
        &chat(),
    )
    .await;

    assert_eq!(harness.api.count("sendMessage"), 1);
    assert_eq!(outcome.posted, None);
}

#[tokio::test]
async fn a_short_rate_limit_is_retried_once() {
    let harness = harness();
    harness.api.rate_limit("sendMessage", 1);
    harness.api.reply(
        "sendMessage",
        CannedAnswer::ok(&json!({ "message_id": 9, "chat": { "id": CHAT, "type": "private" } })),
    );

    let outcome = render(
        &harness,
        &request("the answer", OutboundKind::Reply),
        &chat(),
    )
    .await;

    assert_eq!(harness.api.count("sendMessage"), 2);
    assert_eq!(outcome.posted, Some(9));
}

#[tokio::test]
async fn a_failed_progress_is_never_retried() {
    // It is disposable by construction, and the reply behind it carries the
    // same text.
    let harness = harness();
    harness.api.fail("sendMessage", 400, "Bad Request");

    let outcome = render(
        &harness,
        &request("so far", OutboundKind::Progress),
        &chat(),
    )
    .await;

    assert_eq!(harness.api.count("sendMessage"), 1);
    assert_eq!(outcome.posted, None);
}

#[tokio::test]
async fn a_failed_edit_leaves_the_last_edit_time_alone() {
    let harness = harness();
    harness.api.fail("editMessageText", 400, "Bad Request");
    let mut chat = chat();
    chat.live_message_id = Some(77);
    chat.live_turn_id = Some("turn-1".to_owned());
    chat.last_edit_ms = NOW - 10_000;
    let mut request = request("the answer", OutboundKind::Reply);
    request.turn_id = Some("turn-1".to_owned());

    let outcome = render(&harness, &request, &chat).await;

    assert_eq!(outcome.last_edit_ms, NOW - 10_000);
}

#[tokio::test]
async fn an_edit_that_changed_nothing_is_not_a_fault() {
    // A delta that added no visible characters re-renders to the same string.
    let harness = harness();
    harness.api.fail(
        "editMessageText",
        400,
        "Bad Request: message is not modified",
    );
    let mut chat = chat();
    chat.live_message_id = Some(77);
    chat.live_turn_id = Some("turn-1".to_owned());
    chat.last_edit_ms = NOW - 10_000;
    let mut request = request("the answer", OutboundKind::Reply);
    request.turn_id = Some("turn-1".to_owned());

    let outcome = render(&harness, &request, &chat).await;

    assert_eq!(harness.api.count("editMessageText"), 1);
    assert_eq!(outcome.live_message_id, None);
}

#[tokio::test]
async fn an_edit_records_when_it_landed() {
    let harness = harness();
    let mut chat = chat();
    chat.live_message_id = Some(77);
    chat.live_turn_id = Some("turn-1".to_owned());
    chat.last_edit_ms = NOW - 10_000;
    harness.clock.advance(Duration::from_secs(5));
    let mut request = request("so far", OutboundKind::Progress);
    request.turn_id = Some("turn-1".to_owned());

    let outcome = render(&harness, &request, &chat).await;

    assert_eq!(outcome.last_edit_ms, NOW + 5000);
}

// update

#[tokio::test]
async fn update_rewrites_a_card_this_channel_posted_earlier() {
    let harness = harness();

    harness
        .renderer
        .update(CHAT, 77, "Approved once. Waiting.", true, &live())
        .await;

    let body = &harness.api.bodies("editMessageText")[0];
    assert_eq!(body["message_id"], json!(77));
    assert_eq!(body["text"], json!(r"Approved once\. Waiting\."));
    assert_eq!(body["parse_mode"], json!("MarkdownV2"));
}

#[tokio::test]
async fn update_sends_plain_text_when_the_chat_asked_for_it() {
    let harness = harness();

    harness
        .renderer
        .update(CHAT, 77, "Approved once.", false, &live())
        .await;

    let body = &harness.api.bodies("editMessageText")[0];
    assert_eq!(body["text"], json!("Approved once."));
    assert!(!body.contains_key("parse_mode"));
}

#[tokio::test]
async fn update_treats_an_unchanged_card_as_normal() {
    let harness = harness();
    harness.api.fail(
        "editMessageText",
        400,
        "Bad Request: message is not modified",
    );

    harness
        .renderer
        .update(CHAT, 77, "the same", false, &live())
        .await;

    assert_eq!(harness.api.count("editMessageText"), 1);
}

#[tokio::test]
async fn update_drops_any_other_failure_rather_than_raising_it() {
    let harness = harness();
    harness.api.fail("editMessageText", 403, "Forbidden");

    harness
        .renderer
        .update(CHAT, 77, "gone", false, &live())
        .await;

    assert_eq!(harness.api.count("editMessageText"), 1);
}

#[test]
fn the_renderer_never_shows_its_token_even_when_printed() {
    let harness = harness();
    let rendered = format!("{:?}", harness.renderer);

    assert!(!rendered.contains("secret"), "{rendered}");
    assert!(rendered.contains("telegram"), "{rendered}");
}

#[test]
fn an_outcome_starts_as_nothing_happened() {
    let outcome = RenderOutcome::default();

    assert_eq!(outcome.posted, None);
    assert_eq!(outcome.live_message_id, None);
    assert_eq!(outcome.last_edit_ms, 0);
    assert_eq!(Value::Null, Value::Null);
}
