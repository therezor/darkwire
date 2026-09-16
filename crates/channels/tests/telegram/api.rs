//! The Bot API over its injected transport, including the failures only a
//! broken API produces.

use std::sync::Arc;

use darkwire_channels::telegram::api::{
    BotApi, BotCommand, EditMessageInput, HttpClient, InlineKeyboardButton, InlineKeyboardMarkup,
    ReqwestHttpClient, SendMessageInput, TelegramApiError,
};
use darkwire_core::{ErrorKind, WireError};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::fake_bot_api::{CannedAnswer, FakeBotApi};

const TOKEN: &str = "12345:secret-token";

fn api(fake: &Arc<FakeBotApi>) -> BotApi {
    BotApi::new(
        TOKEN,
        "http://telegram.test/",
        Arc::clone(fake) as Arc<dyn HttpClient>,
    )
}

fn live() -> CancellationToken {
    CancellationToken::new()
}

// The happy path

#[tokio::test]
async fn get_me_confirms_the_token_and_gives_the_username() {
    let fake = FakeBotApi::new();
    let me = api(&fake).get_me(&live()).await.expect("getMe answers");

    assert_eq!(me.id, 1);
    assert_eq!(me.username.as_deref(), Some("ghost_test_bot"));
}

#[tokio::test]
async fn get_updates_asks_only_for_what_this_channel_can_answer() {
    // Asking for more only makes the offset advance over updates nobody reads.
    let fake = FakeBotApi::new();
    fake.reply("getUpdates", CannedAnswer::ok(&json!([])));

    api(&fake)
        .get_updates(7, 30, &live())
        .await
        .expect("getUpdates answers");

    let body = &fake.bodies("getUpdates")[0];
    assert_eq!(body["offset"], json!(7));
    assert_eq!(body["timeout"], json!(30));
    assert_eq!(
        body["allowed_updates"],
        json!(["message", "callback_query"])
    );
}

#[tokio::test]
async fn a_result_that_is_not_a_list_is_no_updates_rather_than_a_failure() {
    // The next poll asks again from the same offset.
    let fake = FakeBotApi::new();
    fake.reply("getUpdates", CannedAnswer::ok(&json!({ "not": "a list" })));

    let updates = api(&fake)
        .get_updates(0, 30, &live())
        .await
        .expect("no failure");

    assert!(updates.is_empty());
}

#[tokio::test]
async fn send_message_carries_the_parse_mode_only_when_it_is_asked_for() {
    let fake = FakeBotApi::new();
    let api = api(&fake);

    api.send_message(
        &SendMessageInput {
            chat_id: 4471,
            text: "hello".to_owned(),
            markdown: true,
            reply_markup: None,
        },
        &live(),
    )
    .await
    .expect("it sends");
    api.send_message(
        &SendMessageInput {
            chat_id: 4471,
            text: "hello".to_owned(),
            markdown: false,
            reply_markup: None,
        },
        &live(),
    )
    .await
    .expect("it sends");

    let bodies = fake.bodies("sendMessage");
    assert_eq!(bodies[0]["parse_mode"], json!("MarkdownV2"));
    // Omitted entirely for the plain-text retry.
    assert!(!bodies[1].contains_key("parse_mode"));
}

#[tokio::test]
async fn send_message_carries_a_keyboard_when_it_has_one() {
    let fake = FakeBotApi::new();

    api(&fake)
        .send_message(
            &SendMessageInput {
                chat_id: 4471,
                text: "pick".to_owned(),
                markdown: false,
                reply_markup: Some(InlineKeyboardMarkup {
                    inline_keyboard: vec![vec![InlineKeyboardButton {
                        text: "one".to_owned(),
                        callback_data: "t1".to_owned(),
                    }]],
                }),
            },
            &live(),
        )
        .await
        .expect("it sends");

    let body = &fake.bodies("sendMessage")[0];
    assert_eq!(
        body["reply_markup"]["inline_keyboard"][0][0]["text"],
        json!("one")
    );
}

#[tokio::test]
async fn edit_message_text_names_the_message_it_rewrites() {
    let fake = FakeBotApi::new();

    api(&fake)
        .edit_message_text(
            &EditMessageInput {
                chat_id: 4471,
                message_id: 77,
                text: "filled in".to_owned(),
                markdown: true,
                reply_markup: None,
            },
            &live(),
        )
        .await
        .expect("it edits");

    let body = &fake.bodies("editMessageText")[0];
    assert_eq!(body["chat_id"], json!(4471));
    assert_eq!(body["message_id"], json!(77));
    assert_eq!(body["text"], json!("filled in"));
}

#[tokio::test]
async fn delete_webhook_and_set_my_commands_are_posted_as_they_stand() {
    let fake = FakeBotApi::new();
    let api = api(&fake);

    api.delete_webhook(&live()).await.expect("it clears");
    api.set_my_commands(
        &[BotCommand {
            command: "help".to_owned(),
            description: "Everything this bot understands".to_owned(),
        }],
        &live(),
    )
    .await
    .expect("it registers");

    assert_eq!(fake.count("deleteWebhook"), 1);
    assert_eq!(
        fake.bodies("setMyCommands")[0]["commands"][0]["command"],
        json!("help")
    );
}

#[tokio::test]
async fn answering_a_callback_query_may_carry_a_reason() {
    let fake = FakeBotApi::new();
    let api = api(&fake);

    api.answer_callback_query("cbq-1", None, &live())
        .await
        .expect("it answers");
    api.answer_callback_query("cbq-2", Some("Not for you."), &live())
        .await
        .expect("it answers");

    let bodies = fake.bodies("answerCallbackQuery");
    assert!(!bodies[0].contains_key("text"));
    assert_eq!(bodies[1]["text"], json!("Not for you."));
}

// Failures

#[tokio::test]
async fn a_two_hundred_with_ok_false_is_still_a_failure() {
    // The API answers `200 {ok: false}` as readily as it answers `400`.
    let fake = FakeBotApi::new();
    fake.reply(
        "getMe",
        CannedAnswer {
            status: Some(200),
            body: Some(json!({ "ok": false, "error_code": 401, "description": "Unauthorized" })),
            ..CannedAnswer::default()
        },
    );

    let error = api(&fake).get_me(&live()).await.expect_err("it fails");
    let api_error = error.api().expect("Telegram answered");

    assert_eq!(api_error.code, 401);
    assert!(api_error.is_unauthorized());
}

#[tokio::test]
async fn the_http_status_stands_in_when_telegram_sends_no_code() {
    let fake = FakeBotApi::new();
    fake.reply(
        "getMe",
        CannedAnswer {
            status: Some(502),
            body: Some(json!({ "ok": false })),
            ..CannedAnswer::default()
        },
    );

    let error = api(&fake).get_me(&live()).await.expect_err("it fails");
    let api_error = error.api().expect("Telegram answered");

    assert_eq!(api_error.code, 502);
    assert_eq!(api_error.description, "HTTP 502");
}

#[tokio::test]
async fn a_body_that_is_not_json_reports_the_status() {
    // A proxy's HTML error page is more usefully reported as its status than as
    // a JSON parse error.
    let fake = FakeBotApi::new();
    fake.reply(
        "getMe",
        CannedAnswer {
            status: Some(503),
            raw: Some("<html>down for maintenance</html>".to_owned()),
            ..CannedAnswer::default()
        },
    );

    let error = api(&fake).get_me(&live()).await.expect_err("it fails");
    let api_error = error.api().expect("Telegram answered");

    assert_eq!(api_error.code, 503);
    assert_eq!(api_error.description, "the response body was not JSON");
}

#[tokio::test]
async fn a_rate_limit_carries_its_retry_after() {
    // Lifted out of `parameters`, where the rate limiter puts it.
    let fake = FakeBotApi::new();
    fake.rate_limit("sendMessage", 12);

    let error = api(&fake)
        .send_message(
            &SendMessageInput {
                chat_id: 1,
                text: "hi".to_owned(),
                markdown: false,
                reply_markup: None,
            },
            &live(),
        )
        .await
        .expect_err("it fails");

    assert_eq!(
        error.api().expect("Telegram answered").retry_after_sec,
        Some(12)
    );
}

#[tokio::test]
async fn a_conflict_is_recognisable_without_reading_a_message() {
    let fake = FakeBotApi::new();
    fake.fail(
        "getUpdates",
        409,
        "Conflict: terminated by other getUpdates",
    );

    let error = api(&fake)
        .get_updates(0, 30, &live())
        .await
        .expect_err("it fails");

    assert!(error.api().expect("Telegram answered").is_conflict());
}

#[tokio::test]
async fn an_edit_that_changed_nothing_is_recognised_as_normal() {
    // A turn whose last delta added no visible text re-renders to the same
    // string, and Telegram calls that a 400.
    let fake = FakeBotApi::new();
    fake.fail(
        "editMessageText",
        400,
        "Bad Request: message is not modified",
    );

    let error = api(&fake)
        .edit_message_text(
            &EditMessageInput {
                chat_id: 1,
                message_id: 1,
                text: "same".to_owned(),
                markdown: false,
                reply_markup: None,
            },
            &live(),
        )
        .await
        .expect_err("it fails");

    assert!(error.api().expect("Telegram answered").is_not_modified());
}

#[tokio::test]
async fn a_response_of_the_wrong_shape_is_reported_rather_than_assumed() {
    let fake = FakeBotApi::new();
    fake.reply("getMe", CannedAnswer::ok(&json!("not a user")));

    let error = api(&fake).get_me(&live()).await.expect_err("it fails");

    assert!(
        error
            .api()
            .expect("Telegram answered")
            .description
            .contains("not the shape"),
        "{error}"
    );
}

#[tokio::test]
async fn a_transport_failure_is_not_a_telegram_failure() {
    let fake = FakeBotApi::new();
    fake.reply(
        "getMe",
        CannedAnswer {
            throws: Some("connection reset".to_owned()),
            ..CannedAnswer::default()
        },
    );

    let error = api(&fake).get_me(&live()).await.expect_err("it fails");

    assert!(error.api().is_none());
    let wire: WireError = error.into();
    assert_eq!(wire.kind, ErrorKind::Network);
}

#[tokio::test]
async fn a_cancelled_token_unwinds_a_call_in_flight() {
    let fake = FakeBotApi::new();
    let token = CancellationToken::new();
    token.cancel();

    let error = api(&fake).get_me(&token).await.expect_err("it aborts");

    assert!(error.is_aborted());
}

#[tokio::test]
async fn a_parked_long_poll_unwinds_at_shutdown() {
    // Rather than holding the process open for its full timeout.
    let fake = FakeBotApi::new();
    let token = CancellationToken::new();
    let api = api(&fake);

    let cancelling = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        cancelling.cancel();
    });

    let error = api.get_updates(0, 30, &token).await.expect_err("it aborts");
    assert!(error.is_aborted());
}

// What an error carries, and what it must not

#[test]
fn an_error_names_the_method_never_the_url() {
    // The URL contains the bot token.
    let error = TelegramApiError {
        code: 400,
        retry_after_sec: None,
        method: "sendMessage".to_owned(),
        description: "Bad Request".to_owned(),
    };

    let rendered = error.to_string();
    assert!(rendered.contains("sendMessage"), "{rendered}");
    assert!(!rendered.contains("api.telegram.org"), "{rendered}");
    assert!(!rendered.contains(TOKEN), "{rendered}");
}

#[tokio::test]
async fn nothing_a_failure_carries_leaks_the_token() {
    let fake = FakeBotApi::new();
    fake.fail("sendMessage", 400, "Bad Request");

    let error = api(&fake)
        .send_message(
            &SendMessageInput {
                chat_id: 1,
                text: "hi".to_owned(),
                markdown: false,
                reply_markup: None,
            },
            &live(),
        )
        .await
        .expect_err("it fails");

    let wire: WireError = error.into();
    let rendered = format!("{} {:?}", wire.message, wire.details);
    assert!(!rendered.contains(TOKEN), "{rendered}");
    assert_eq!(wire.details["method"], json!("sendMessage"));
    assert_eq!(wire.details["code"], json!(400));
}

#[test]
fn a_telegram_failure_becomes_the_error_kind_it_means() {
    let kinds = [
        (401, ErrorKind::PermissionDenied),
        (403, ErrorKind::PermissionDenied),
        (409, ErrorKind::Conflict),
        (429, ErrorKind::RateLimited),
        (500, ErrorKind::Network),
    ];
    for (code, expected) in kinds {
        let wire: WireError = TelegramApiError {
            code,
            retry_after_sec: Some(3),
            method: "getMe".to_owned(),
            description: "nope".to_owned(),
        }
        .into();
        assert_eq!(wire.kind, expected, "{code}");
        assert_eq!(wire.details["retryAfterSec"], json!(3));
    }
}

#[test]
fn the_api_never_shows_its_token_even_when_printed() {
    let fake = FakeBotApi::new();
    let rendered = format!("{:?}", api(&fake));

    assert!(!rendered.contains(TOKEN), "{rendered}");
}

// The real transport

#[tokio::test]
async fn the_reqwest_transport_posts_json_and_reads_the_answer_whole() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/getMe")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": { "id": 9, "username": "real_bot" },
        })))
        .mount(&server)
        .await;

    let http = Arc::new(ReqwestHttpClient::new().expect("a client")) as Arc<dyn HttpClient>;
    let me = BotApi::new(TOKEN, &server.uri(), http)
        .get_me(&live())
        .await
        .expect("getMe answers");

    assert_eq!(me.id, 9);
    assert_eq!(me.username.as_deref(), Some("real_bot"));
}

#[tokio::test]
async fn the_reqwest_transport_reports_an_unreachable_host_without_the_url() {
    let http = Arc::new(ReqwestHttpClient::new().expect("a client")) as Arc<dyn HttpClient>;
    // A port nothing is listening on, so the connection is refused outright.
    let error = BotApi::new(TOKEN, "http://127.0.0.1:1", http)
        .get_me(&live())
        .await
        .expect_err("it fails");

    let wire: WireError = error.into();
    assert_eq!(wire.kind, ErrorKind::Network);
    assert!(!wire.message.contains(TOKEN), "{}", wire.message);
}

#[tokio::test]
async fn the_reqwest_transport_honours_a_token_that_has_already_fired() {
    let http = ReqwestHttpClient::new().expect("a client");
    let token = CancellationToken::new();
    token.cancel();

    let error = http
        .post_json("http://127.0.0.1:1/x", Value::Null.to_string(), &token)
        .await
        .expect_err("it aborts");

    assert!(error.is_aborted());
}
