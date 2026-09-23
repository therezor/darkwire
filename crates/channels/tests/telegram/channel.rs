//! The lifecycle and the routing: a long poll, an allowlist, and the four
//! things a channel does.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use darkwire_channels::channel::Channel;
use darkwire_channels::manager::{ChannelHub, ChannelManager, ChannelManagerOptions};
use darkwire_channels::projection::{
    APPROVAL_ERROR_METADATA_KEY, APPROVAL_METADATA_KEY, APPROVAL_SETTLED_METADATA_KEY,
};
use darkwire_channels::telegram::api::HttpClient;
use darkwire_channels::telegram::channel::{TelegramChannelOptions, telegram_channel};
use darkwire_channels::telegram::console::TelegramConsole;
use darkwire_channels::testkit::{ScriptedHub, counter_ids, flush};
use darkwire_core::ErrorKind;
use darkwire_core::message_bus::{
    MessageBus, MessageBusOptions, OutboundKind, OutboundMessageInput, RateLimitOptions,
};
use darkwire_core::messages::text_part;
use darkwire_core::session_store::CreateSession;
use darkwire_protocol::ApprovalScope;
use serde_json::{Map, Value, json};

use super::console_double::FakeConsole;
use super::fake_bot_api::{CannedAnswer, FakeBotApi, callback_update, message_update};

const USER: i64 = 4471;
const OTHER: i64 = 9999;
const GROUP: i64 = -100_123;

struct Bot {
    manager: ChannelManager,
    api: Arc<FakeBotApi>,
    console: Arc<FakeConsole>,
    hub: Arc<ScriptedHub>,
}

impl Bot {
    fn channel(&self) -> Arc<dyn Channel> {
        self.manager
            .channel("telegram")
            .expect("the channel started")
    }
}

fn settings(value: Value) -> Map<String, Value> {
    let mut channels = Map::new();
    channels.insert("telegram".to_owned(), value);
    channels
}

fn allowlisted() -> Value {
    json!({ "allowlist": [USER.to_string(), GROUP.to_string()], "pollTimeoutSec": 1 })
}

/// A manager over the Telegram channel, not yet started.
fn build(
    api: &Arc<FakeBotApi>,
    console: &Arc<FakeConsole>,
    hub: &Arc<ScriptedHub>,
    block: Value,
    bus: Option<Arc<MessageBus>>,
) -> ChannelManager {
    let ids = Arc::new(AtomicUsize::new(0));
    let factory = telegram_channel(TelegramChannelOptions {
        token: "12345:secret".to_owned(),
        console: Arc::clone(console) as Arc<dyn TelegramConsole>,
        new_id: Arc::new(move || format!("id{}", ids.fetch_add(1, Ordering::SeqCst))),
        http: Some(Arc::clone(api) as Arc<dyn HttpClient>),
        id: "telegram".to_owned(),
    });
    ChannelManager::new(ChannelManagerOptions {
        factories: vec![factory],
        channels: settings(block),
        bus,
        ..ChannelManagerOptions::new(
            Arc::clone(hub) as Arc<dyn ChannelHub>,
            counter_ids("telegram-"),
        )
    })
    .expect("the manager takes the factory")
}

async fn start(block: Value, hub: Arc<ScriptedHub>) -> Bot {
    let api = FakeBotApi::new();
    let console = FakeConsole::new().expect("the stores open");
    let manager = build(&api, &console, &hub, block, None);
    manager.start().await.expect("the channel connects");
    Bot {
        manager,
        api,
        console,
        hub,
    }
}

async fn bot() -> Bot {
    start(allowlisted(), ScriptedHub::silent()).await
}

// Starting

#[tokio::test]
async fn an_empty_allowlist_refuses_to_start() {
    // A bot that comes up answering nobody looks exactly like one with a broken
    // token, and one that comes up answering anybody is a shell on this machine.
    let api = FakeBotApi::new();
    let console = FakeConsole::new().expect("the stores open");
    let manager = build(&api, &console, &ScriptedHub::silent(), json!({}), None);

    let error = manager.start().await.expect_err("it refuses");

    assert_eq!(error.kind, ErrorKind::Config);
    assert!(
        error.message.contains("allowlist is empty"),
        "{}",
        error.message
    );
    // It never reached the network.
    assert_eq!(api.count("getMe"), 0);
}

#[tokio::test]
async fn a_bad_token_fails_the_start_rather_than_dying_quietly() {
    let api = FakeBotApi::new();
    api.fail("getMe", 401, "Unauthorized");
    let console = FakeConsole::new().expect("the stores open");
    let manager = build(&api, &console, &ScriptedHub::silent(), allowlisted(), None);

    let error = manager.start().await.expect_err("it fails");

    assert_eq!(error.kind, ErrorKind::PermissionDenied);
    assert!(manager.channels().is_empty());
}

#[tokio::test]
async fn starting_clears_a_stale_webhook_and_registers_the_menu() {
    // The commonest 409 is a webhook left over from an earlier setup, and it
    // looks exactly like the serious one.
    let bot = bot().await;

    assert_eq!(bot.api.count("getMe"), 1);
    assert_eq!(bot.api.count("deleteWebhook"), 1);
    let registered = &bot.api.bodies("setMyCommands")[0]["commands"];
    assert!(
        registered
            .as_array()
            .expect("a list")
            .iter()
            .any(|command| command["command"] == json!("help"))
    );

    bot.manager.stop().await;
}

#[tokio::test]
async fn start_returns_rather_than_running_the_poll_inline() {
    // The manager awaits `start()`, so a method that ran the poll inline would
    // never return and the server would never finish booting.
    let bot = bot().await;

    assert_eq!(bot.manager.channels().len(), 1);
    flush().await;
    // The loop is running: it has asked for updates without anybody waiting.
    assert!(bot.api.count("getUpdates") >= 1);

    bot.manager.stop().await;
}

#[tokio::test]
async fn the_channel_declares_progress_so_a_turn_fills_one_message_in() {
    let bot = bot().await;

    let accepts = bot.channel().accepts().to_vec();

    assert!(accepts.contains(&OutboundKind::Progress));
    assert!(accepts.contains(&OutboundKind::Reply));
    assert!(accepts.contains(&OutboundKind::Notice));
    assert!(accepts.contains(&OutboundKind::Error));

    bot.manager.stop().await;
}

// Inbound messages

#[tokio::test]
async fn an_ordinary_message_is_published_with_the_chat_as_its_reply_address() {
    let bot = bot().await;

    bot.api.push(message_update("hello there", USER, None));
    flush().await;

    let messages = bot.hub.messages();
    let frame = messages.first().expect("one frame reached the hub");
    assert_eq!(frame.content.as_deref(), Some("hello there"));
    // The Telegram message id, so a redelivered update is acked rather than run
    // twice.
    assert_eq!(frame.client_message_id.as_deref(), Some("4471:1"));
    assert_eq!(bot.hub.only().session_key(), "telegram:4471");

    bot.manager.stop().await;
}

#[tokio::test]
async fn a_stranger_is_dropped_silently() {
    // A reply confirms the bot is live and spends the rate limit on whoever is
    // knocking.
    let bot = bot().await;

    bot.api.push(message_update("let me in", OTHER, None));
    flush().await;

    assert!(bot.hub.messages().is_empty());
    assert_eq!(bot.api.count("sendMessage"), 0);

    bot.manager.stop().await;
}

#[tokio::test]
async fn being_in_a_group_the_bot_is_in_is_not_permission() {
    let bot = bot().await;

    bot.api
        .push(message_update("let me in", OTHER, Some(GROUP)));
    flush().await;
    assert!(bot.hub.messages().is_empty());

    // The person typing has to be listed too, and this one is.
    bot.api.push(message_update("hello", USER, Some(GROUP)));
    flush().await;
    assert_eq!(bot.hub.messages().len(), 1);

    bot.manager.stop().await;
}

#[tokio::test]
async fn a_message_with_no_text_is_nothing_to_do() {
    let bot = bot().await;
    let mut update = message_update("", USER, None);
    if let Some(message) = update.message.as_mut() {
        message.text = None;
    }

    bot.api.push(update);
    flush().await;

    assert!(bot.hub.messages().is_empty());
    bot.manager.stop().await;
}

#[tokio::test]
async fn a_sender_the_bot_cannot_see_is_nothing_to_do() {
    let bot = bot().await;
    let mut update = message_update("hello", USER, None);
    if let Some(message) = update.message.as_mut() {
        message.from = None;
    }

    bot.api.push(update);
    flush().await;

    assert!(bot.hub.messages().is_empty());
    bot.manager.stop().await;
}

#[tokio::test]
async fn a_rate_limited_message_is_answered_rather_than_dropped() {
    let bus = Arc::new(MessageBus::new(MessageBusOptions {
        rate_limit: RateLimitOptions {
            per_minute: 1.0,
            burst: Some(1),
        },
        ..MessageBusOptions::new(
            Arc::new(darkwire_core::clock::SystemClock),
            counter_ids("bus-"),
        )
    }));
    let api = FakeBotApi::new();
    let console = FakeConsole::new().expect("the stores open");
    let hub = ScriptedHub::silent();
    let manager = build(&api, &console, &hub, allowlisted(), Some(Arc::clone(&bus)));
    manager.start().await.expect("it connects");

    api.push(message_update("one", USER, None));
    flush().await;
    api.push(message_update("two", USER, None));
    flush().await;

    assert!(api.said("Slow down a moment"), "{:?}", api.texts());
    manager.stop().await;
}

#[tokio::test]
async fn a_command_is_answered_rather_than_published() {
    let bot = bot().await;

    bot.api.push(message_update("/help", USER, None));
    flush().await;

    assert!(bot.hub.messages().is_empty());
    assert!(
        bot.api.said("These are the commands"),
        "{:?}",
        bot.api.texts()
    );

    bot.manager.stop().await;
}

#[tokio::test]
async fn a_command_moves_this_chat_to_another_conversation() {
    // Switching conversation is publishing a different key: the manager needs
    // no notion of a switch at all.
    let bot = bot().await;

    bot.api.push(message_update("/new", USER, None));
    flush().await;
    bot.api.push(message_update("hello", USER, None));
    flush().await;

    assert_eq!(bot.hub.only().session_key(), "telegram:4471:id0");
    bot.manager.stop().await;
}

#[tokio::test]
async fn an_admin_verb_reads_the_person_typing_not_the_room() {
    // In a group those differ, and reading the chat id as a user id would hand
    // an admin verb to anybody in a room whose id is on the admin list.
    let block = json!({
        "allowlist": [USER.to_string(), GROUP.to_string(), OTHER.to_string()],
        "admins": [GROUP.to_string()],
        "pollTimeoutSec": 1,
    });
    let bot = start(block, ScriptedHub::silent()).await;

    bot.api.push(message_update("/model o3", USER, Some(GROUP)));
    flush().await;

    assert!(bot.api.said("administrator"), "{:?}", bot.api.texts());
    assert!(bot.console.models_set().is_empty());

    bot.manager.stop().await;
}

// Button presses

#[tokio::test]
async fn a_press_from_a_stranger_is_refused_and_answered() {
    // An approval answered from an inline keyboard is an authorisation decision
    // arriving from an unauthenticated source unless it goes through the
    // allowlist too.
    let bot = bot().await;

    bot.api.push(callback_update("t1", OTHER, None));
    flush().await;

    let answered = &bot.api.bodies("answerCallbackQuery")[0];
    assert_eq!(answered["text"], json!("Not for you."));

    bot.manager.stop().await;
}

#[tokio::test]
async fn a_button_from_a_menu_that_is_gone_says_so() {
    let bot = bot().await;

    bot.api.push(callback_update("nope", USER, None));
    flush().await;

    let answered = &bot.api.bodies("answerCallbackQuery")[0];
    assert_eq!(answered["text"], json!("That menu has expired. Ask again."));

    bot.manager.stop().await;
}

#[tokio::test]
async fn a_press_is_always_answered_so_the_button_stops_spinning() {
    let bot = bot().await;
    bot.api.push(message_update("/agent", USER, None));
    flush().await;
    let token = first_token(&bot);

    bot.api.push(callback_update(&token, USER, None));
    flush().await;

    assert!(bot.api.count("answerCallbackQuery") >= 1);
    bot.manager.stop().await;
}

/// The token on the first button of the last keyboard the bot posted.
fn first_token(bot: &Bot) -> String {
    bot.api
        .calls()
        .iter()
        .rev()
        .find_map(|call| {
            call.body
                .get("reply_markup")?
                .get("inline_keyboard")?
                .get(0)?
                .get(0)?
                .get("callback_data")?
                .as_str()
                .map(str::to_owned)
        })
        .expect("a keyboard was posted")
}

#[tokio::test]
async fn choosing_an_agent_binds_the_session_and_says_so_on_the_card() {
    // The card becomes its own outcome rather than a second message under it.
    let bot = bot().await;
    bot.api.push(message_update("/agent", USER, None));
    flush().await;
    let token = first_token(&bot);

    bot.api.push(callback_update(&token, USER, None));
    flush().await;

    assert_eq!(
        bot.console
            .store()
            .get_session("telegram:4471")
            .expect("get")
            .expect("created")
            .agent_id
            .as_deref(),
        Some("default")
    );
    assert!(bot.api.said("now runs on"), "{:?}", bot.api.texts());

    bot.manager.stop().await;
}

#[tokio::test]
async fn choosing_a_model_is_refused_for_a_non_administrator() {
    let block = json!({
        "allowlist": [USER.to_string(), OTHER.to_string()],
        "admins": [USER.to_string()],
        "pollTimeoutSec": 1,
    });
    let bot = start(block, ScriptedHub::silent()).await;
    bot.api.push(message_update("/model", USER, None));
    flush().await;
    let token = first_token(&bot);

    // Posted in the admin's chat, pressed by somebody else who is nonetheless
    // allowed to be there.
    bot.api.push(callback_update(&token, OTHER, Some(USER)));
    flush().await;

    assert!(bot.console.models_set().is_empty());
    assert!(bot.api.said("administrator"), "{:?}", bot.api.texts());

    bot.manager.stop().await;
}

#[tokio::test]
async fn deleting_a_session_from_its_row_takes_two_taps_and_detaches_this_chat() {
    // `/delete` drops the last exchange now, so deleting the conversation is a
    // bin beside its name in `/session` — where the thing being deleted is
    // named and counted in front of you. Two taps, because a fingertip lands
    // on the wrong row of a scrolling list easily and this cannot be undone.
    let bot = bot().await;
    bot.console
        .store()
        .ensure_session(
            "telegram:4471",
            CreateSession {
                origin: Some("telegram".to_owned()),
                ..CreateSession::default()
            },
        )
        .expect("created");
    bot.api.push(message_update("/session", USER, None));
    flush().await;
    // The bin sits second, after the button that attaches.
    let bin = nth_token(&bot, 1);

    bot.api.push(callback_update(&bin, USER, None));
    flush().await;
    assert!(
        bot.api.said("This cannot be undone"),
        "{:?}",
        bot.api.texts()
    );

    let confirm = last_token(&bot);
    bot.api.push(callback_update(&confirm, USER, None));
    flush().await;

    assert!(
        bot.console
            .store()
            .get_session("telegram:4471")
            .expect("get")
            .is_none()
    );
    assert!(bot.api.said("Deleted"), "{:?}", bot.api.texts());

    bot.manager.stop().await;
}

#[tokio::test]
async fn a_paging_button_rebuilds_the_listing_rather_than_remembering_it() {
    let bot = bot().await;
    for index in 0..12 {
        bot.console
            .store()
            .ensure_session(
                &format!("telegram:4471:s{index}"),
                CreateSession {
                    origin: Some("telegram".to_owned()),
                    ..CreateSession::default()
                },
            )
            .expect("created");
    }
    bot.api.push(message_update("/session", USER, None));
    flush().await;
    // The last button of the last row is the `Next »` arrow.
    let next = last_token(&bot);

    let before = bot.api.count("sendMessage");
    bot.api.push(callback_update(&next, USER, None));
    flush().await;

    assert!(bot.api.count("sendMessage") > before);
    assert!(bot.api.said("Which one?"), "{:?}", bot.api.texts());

    bot.manager.stop().await;
}

/// The token on the last button of the last keyboard the bot posted — the
/// The `at`th button of the most recent keyboard, reading left to right.
fn nth_token(bot: &Bot, at: usize) -> String {
    bot.api
        .calls()
        .iter()
        .rev()
        .find_map(|call| {
            let rows = call
                .body
                .get("reply_markup")?
                .get("inline_keyboard")?
                .as_array()?;
            let buttons: Vec<&serde_json::Value> = rows
                .iter()
                .filter_map(|row| row.as_array())
                .flatten()
                .collect();
            buttons
                .get(at)?
                .get("callback_data")?
                .as_str()
                .map(str::to_owned)
        })
        .expect("a keyboard was posted")
}

/// `Next »` arrow, when the listing has one.
fn last_token(bot: &Bot) -> String {
    bot.api
        .calls()
        .iter()
        .rev()
        .find_map(|call| {
            let rows = call
                .body
                .get("reply_markup")?
                .get("inline_keyboard")?
                .as_array()?;
            let last = rows.last()?.as_array()?;
            last.last()?
                .get("callback_data")?
                .as_str()
                .map(str::to_owned)
        })
        .expect("a keyboard was posted")
}

#[tokio::test]
async fn a_page_of_a_short_listing_is_re_asked_rather_than_paged() {
    // `models` and `workspaces` are short enough that a second `/model` costs
    // less than the state.
    let bot = bot().await;
    let many: Vec<String> = (0..20).map(|index| format!("model-{index}")).collect();
    let ids: Vec<&str> = many.iter().map(String::as_str).collect();
    bot.console.set_models(&ids);
    bot.api.push(message_update("/model", USER, None));
    flush().await;
    let next = last_token(&bot);

    bot.api.push(callback_update(&next, USER, None));
    flush().await;

    assert!(
        bot.api.said("Ask again for the next page"),
        "{:?}",
        bot.api.texts()
    );
    bot.manager.stop().await;
}

// Outbound

async fn deliver(bot: &Bot, kind: OutboundKind, text: &str, metadata: Map<String, Value>) {
    bot.manager.bus().publish_outbound(OutboundMessageInput {
        channel_id: "telegram".to_owned(),
        session_key: "telegram:4471".to_owned(),
        target: USER.to_string(),
        content: vec![text_part(text)],
        kind,
        metadata,
        id: None,
    });
    flush().await;
}

#[tokio::test]
async fn an_answer_is_rendered_into_the_chat_it_is_addressed_to() {
    let bot = bot().await;

    deliver(&bot, OutboundKind::Reply, "the answer", Map::new()).await;

    let body = &bot.api.bodies("sendMessage")[0];
    assert_eq!(body["chat_id"], json!(USER));
    assert_eq!(body["text"], json!("the answer"));

    bot.manager.stop().await;
}

#[tokio::test]
async fn a_message_whose_target_is_not_a_chat_id_is_dropped() {
    let bot = bot().await;

    bot.manager.bus().publish_outbound(OutboundMessageInput {
        channel_id: "telegram".to_owned(),
        session_key: "telegram:4471".to_owned(),
        target: "not-a-number".to_owned(),
        content: vec![text_part("nowhere")],
        kind: OutboundKind::Reply,
        metadata: Map::new(),
        id: None,
    });
    flush().await;

    assert_eq!(bot.api.count("sendMessage"), 0);
    bot.manager.stop().await;
}

#[tokio::test]
async fn an_approval_grows_buttons_and_carries_nothing_the_model_wrote() {
    let bot = bot().await;
    let mut metadata = Map::new();
    metadata.insert(
        APPROVAL_METADATA_KEY.to_owned(),
        json!({
            "callId": "call-1",
            "name": "exec",
            "risk": "exec",
            "expiresAtMs": 1_700_000_060_000_i64,
        }),
    );

    deliver(
        &bot,
        OutboundKind::Notice,
        "exec needs approval before it can run.",
        metadata,
    )
    .await;

    let body = &bot.api.bodies("sendMessage")[0];
    let text = body["text"].as_str().expect("text");
    assert!(text.contains("🔐"), "{text}");
    assert!(text.contains("exec"), "{text}");
    assert!(text.contains("risk: exec"), "{text}");
    let rows = body["reply_markup"]["inline_keyboard"]
        .as_array()
        .expect("a keyboard");
    assert_eq!(rows.len(), 2);

    bot.manager.stop().await;
}

#[tokio::test]
async fn answering_an_approval_sends_the_frame_a_browser_would_have() {
    let bot = bot().await;
    let mut metadata = Map::new();
    metadata.insert(
        APPROVAL_METADATA_KEY.to_owned(),
        json!({
            "callId": "call-1",
            "name": "exec",
            "risk": "exec",
            "expiresAtMs": 4_000_000_000_000_i64,
        }),
    );
    deliver(&bot, OutboundKind::Notice, "needs approval", metadata).await;
    let token = first_token(&bot);

    bot.api.push(callback_update(&token, USER, None));
    flush().await;

    let frames = bot.hub.only().frames();
    let approve = frames
        .iter()
        .find(|frame| frame.tag == "tool.approve")
        .expect("an approval frame");
    let darkwire_protocol::ClientMessage::ToolApprove(body) = &approve.frame else {
        panic!("a tool.approve");
    };
    assert_eq!(body.call_id, "call-1");
    assert!(body.approved);
    assert_eq!(body.scope, ApprovalScope::Once);
    assert!(bot.api.said("Approved once"), "{:?}", bot.api.texts());

    bot.manager.stop().await;
}

fn exec_approval(argv: &[&str]) -> Map<String, Value> {
    let mut metadata = Map::new();
    metadata.insert(
        APPROVAL_METADATA_KEY.to_owned(),
        json!({
            "callId": "call-1",
            "name": "exec",
            "risk": "exec",
            "expiresAtMs": 4_000_000_000_000_i64,
            "command": { "argv": argv, "shell": argv.first() == Some(&"sh") },
        }),
    );
    metadata
}

fn marker(key: &str) -> Map<String, Value> {
    let mut metadata = Map::new();
    metadata.insert(key.to_owned(), json!({ "callId": "call-1" }));
    metadata
}

/// The token on the button labelled `label` in the last keyboard posted.
fn token_labelled(bot: &Bot, label: &str) -> String {
    bot.api
        .calls()
        .iter()
        .rev()
        .find_map(|call| {
            call.body
                .get("reply_markup")?
                .get("inline_keyboard")?
                .as_array()?
                .iter()
                .flat_map(|row| row.as_array().into_iter().flatten())
                .find(|button| {
                    button["text"]
                        .as_str()
                        .is_some_and(|text| text.starts_with(label))
                })?
                .get("callback_data")?
                .as_str()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| panic!("no button labelled {label}"))
}

#[tokio::test]
async fn an_exec_card_shows_the_command_and_offers_a_rule() {
    let bot = bot().await;

    deliver(
        &bot,
        OutboundKind::Notice,
        "needs approval",
        exec_approval(&["git", "log", "-5"]),
    )
    .await;

    let body = &bot.api.bodies("sendMessage")[0];
    let text = body["text"].as_str().expect("text");
    assert!(text.contains("command: `git log -5`"), "{text}");
    let labels: Vec<&str> = body["reply_markup"]["inline_keyboard"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|row| row.as_array().unwrap())
        .map(|button| button["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        labels,
        vec![
            "✅ Once",
            "✅ This session",
            "✅ Always: git log -5 *",
            "⛔ Deny"
        ]
    );

    bot.manager.stop().await;
}

#[tokio::test]
async fn a_long_command_is_cut_before_it_reaches_the_chat() {
    // A card can land in a group, and a command can carry a token.
    let bot = bot().await;
    let secret = "x".repeat(200);

    deliver(
        &bot,
        OutboundKind::Notice,
        "needs approval",
        exec_approval(&["curl", "-H", &secret]),
    )
    .await;

    let text = bot.api.bodies("sendMessage")[0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(text.contains('…'), "{text}");
    assert!(!text.contains(&secret), "{text}");

    bot.manager.stop().await;
}

#[tokio::test]
async fn a_shell_is_never_offered_a_rule() {
    let bot = bot().await;

    deliver(
        &bot,
        OutboundKind::Notice,
        "needs approval",
        exec_approval(&["sh", "-c", "ls"]),
    )
    .await;

    let rows = bot.api.bodies("sendMessage")[0]["reply_markup"]["inline_keyboard"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(rows, 2);

    bot.manager.stop().await;
}

#[tokio::test]
async fn always_sends_the_rule_with_the_answer() {
    let bot = bot().await;
    deliver(
        &bot,
        OutboundKind::Notice,
        "needs approval",
        exec_approval(&["git", "log"]),
    )
    .await;

    bot.api.push(callback_update(
        &token_labelled(&bot, "✅ Always"),
        USER,
        None,
    ));
    flush().await;

    let frames = bot.hub.only().frames();
    let darkwire_protocol::ClientMessage::ToolApprove(body) = &frames
        .iter()
        .find(|frame| frame.tag == "tool.approve")
        .expect("an approval frame")
        .frame
    else {
        panic!("a tool.approve");
    };
    assert_eq!(body.scope, ApprovalScope::Session);
    assert_eq!(
        body.rule.as_ref().map(|rule| rule.argv.clone()),
        Some(vec!["git".to_owned(), "log".to_owned(), "*".to_owned()])
    );
    assert!(bot.api.said("always allowed"), "{:?}", bot.api.texts());

    bot.manager.stop().await;
}

#[tokio::test]
async fn a_card_answered_elsewhere_loses_its_buttons() {
    let bot = bot().await;
    deliver(
        &bot,
        OutboundKind::Notice,
        "needs approval",
        exec_approval(&["ls"]),
    )
    .await;
    let once = token_labelled(&bot, "✅ Once");

    deliver(
        &bot,
        OutboundKind::Update,
        "No longer waiting for an answer.",
        marker(APPROVAL_SETTLED_METADATA_KEY),
    )
    .await;

    let edit = &bot.api.bodies("editMessageText")[0];
    // The fake numbers posted messages from 101.
    assert_eq!(edit["message_id"], json!(101));
    assert!(edit["text"].as_str().unwrap().contains("No longer waiting"));
    assert!(!edit.contains_key("reply_markup"));
    assert_eq!(bot.api.count("sendMessage"), 1);

    // A late press on the old card does nothing.
    bot.api.push(callback_update(&once, USER, None));
    flush().await;
    assert!(
        bot.hub.connections().iter().all(|connection| connection
            .frames()
            .iter()
            .all(|frame| frame.tag != "tool.approve")),
        "a settled card must not answer"
    );

    bot.manager.stop().await;
}

#[tokio::test]
async fn a_denial_edits_the_card_rather_than_posting_under_it() {
    let bot = bot().await;
    deliver(
        &bot,
        OutboundKind::Notice,
        "needs approval",
        exec_approval(&["ls"]),
    )
    .await;

    deliver(
        &bot,
        OutboundKind::Notice,
        "Denied \"exec\": the approval request expired before it was answered.",
        marker(APPROVAL_SETTLED_METADATA_KEY),
    )
    .await;

    assert_eq!(bot.api.count("sendMessage"), 1);
    let edit = &bot.api.bodies("editMessageText")[0];
    assert!(
        edit["text"].as_str().unwrap().contains("expired"),
        "{edit:?}"
    );

    bot.manager.stop().await;
}

#[tokio::test]
async fn an_update_for_a_card_this_channel_never_posted_is_dropped() {
    let bot = bot().await;

    deliver(
        &bot,
        OutboundKind::Update,
        "No longer waiting for an answer.",
        marker(APPROVAL_SETTLED_METADATA_KEY),
    )
    .await;

    assert_eq!(bot.api.count("sendMessage"), 0);
    assert_eq!(bot.api.count("editMessageText"), 0);
    bot.manager.stop().await;
}

#[tokio::test]
async fn a_card_answered_here_ends_on_the_button_that_was_pressed() {
    let bot = bot().await;
    deliver(
        &bot,
        OutboundKind::Notice,
        "needs approval",
        exec_approval(&["ls"]),
    )
    .await;
    bot.api.push(callback_update(
        &token_labelled(&bot, "✅ Once"),
        USER,
        None,
    ));
    flush().await;

    deliver(
        &bot,
        OutboundKind::Update,
        "No longer waiting for an answer.",
        marker(APPROVAL_SETTLED_METADATA_KEY),
    )
    .await;

    let edits = bot.api.bodies("editMessageText");
    let last = edits.last().unwrap()["text"].as_str().unwrap().to_owned();
    assert!(last.contains("Approved once"), "{last}");
    assert!(!last.contains("Waiting"), "{last}");

    bot.manager.stop().await;
}

#[tokio::test]
async fn a_refused_rule_puts_the_buttons_back_without_it() {
    let bot = bot().await;
    deliver(
        &bot,
        OutboundKind::Notice,
        "needs approval",
        exec_approval(&["git", "log"]),
    )
    .await;
    bot.api.push(callback_update(
        &token_labelled(&bot, "✅ Always"),
        USER,
        None,
    ));
    flush().await;

    deliver(
        &bot,
        OutboundKind::Error,
        "The rule \"git *\" is more specific and would still apply to this command.",
        marker(APPROVAL_ERROR_METADATA_KEY),
    )
    .await;

    assert_eq!(bot.api.count("sendMessage"), 1);
    let edit = bot.api.bodies("editMessageText").last().unwrap().clone();
    assert!(
        edit["text"].as_str().unwrap().contains("more specific"),
        "{edit:?}"
    );
    let labels: Vec<String> = edit["reply_markup"]["inline_keyboard"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|row| row.as_array().unwrap())
        .map(|button| button["text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(labels, vec!["✅ Once", "✅ This session", "⛔ Deny"]);

    bot.manager.stop().await;
}

#[tokio::test]
async fn a_malformed_approval_detail_renders_as_an_ordinary_notice() {
    // A channel that refused to render an approval because one field was the
    // wrong shape would hang the turn it was meant to unblock.
    let bot = bot().await;
    let mut metadata = Map::new();
    metadata.insert(APPROVAL_METADATA_KEY.to_owned(), json!({ "name": "exec" }));

    deliver(&bot, OutboundKind::Notice, "needs approval", metadata).await;

    let body = &bot.api.bodies("sendMessage")[0];
    assert!(!body.contains_key("reply_markup"));
    assert!(
        body["text"]
            .as_str()
            .expect("text")
            .contains("needs approval"),
        "{body:?}"
    );

    bot.manager.stop().await;
}

// The poll loop

#[tokio::test(start_paused = true)]
async fn a_failed_poll_backs_off_rather_than_spinning() {
    let api = FakeBotApi::new();
    api.fail("getUpdates", 500, "Internal Server Error");
    let console = FakeConsole::new().expect("the stores open");
    let manager = build(&api, &console, &ScriptedHub::silent(), allowlisted(), None);
    manager.start().await.expect("it connects");

    // One second of virtual time is the first backoff, so the loop has asked
    // again a handful of times rather than as fast as the runtime allows.
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let attempts = api.count("getUpdates");

    assert!(
        (2..=6).contains(&attempts),
        "{attempts} attempts in four seconds"
    );
    manager.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_conflict_that_survived_deleting_the_webhook_is_a_second_poller() {
    // There is no clever recovery — the two would take turns stealing each
    // other's updates — and the log line is the fix.
    let api = FakeBotApi::new();
    api.fail(
        "getUpdates",
        409,
        "Conflict: terminated by other getUpdates",
    );
    let console = FakeConsole::new().expect("the stores open");
    let manager = build(&api, &console, &ScriptedHub::silent(), allowlisted(), None);
    manager.start().await.expect("it connects");

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    assert!(api.count("getUpdates") >= 1);
    // The channel is still up: a second poller is an operator problem, not a
    // reason to tear the process down.
    assert_eq!(manager.channels().len(), 1);
    manager.stop().await;
}

#[tokio::test]
async fn the_offset_advances_before_an_update_is_handled() {
    // An update that failed its way out would otherwise be redelivered for ever.
    let bot = bot().await;

    bot.api.push(message_update("hello", USER, None));
    flush().await;
    // The fake answers an empty list once drained, so the next poll carries the
    // advanced offset.
    flush().await;

    let offsets: Vec<i64> = bot
        .api
        .bodies("getUpdates")
        .iter()
        .filter_map(|body| body.get("offset")?.as_i64())
        .collect();
    assert!(offsets.iter().any(|offset| *offset > 0), "{offsets:?}");

    bot.manager.stop().await;
}

#[tokio::test]
async fn stopping_confirms_the_last_batch_so_a_restart_does_not_replay_it() {
    let bot = bot().await;
    bot.api.push(message_update("/help", USER, None));
    flush().await;

    bot.manager.stop().await;

    let last = bot.api.bodies("getUpdates").pop().expect("a poll ran");
    assert_eq!(last["timeout"], json!(0));
    assert!(last["offset"].as_i64().unwrap_or_default() > 0, "{last:?}");
}

#[tokio::test]
async fn stopping_before_anything_arrived_asks_telegram_for_nothing() {
    let bot = bot().await;
    flush().await;

    bot.manager.stop().await;

    assert!(
        bot.api
            .bodies("getUpdates")
            .iter()
            .all(|body| body["timeout"] != json!(0))
    );
}

#[tokio::test]
async fn stopping_awaits_the_poll_rather_than_racing_it() {
    let bot = bot().await;
    flush().await;

    bot.manager.stop().await;

    // Nothing more is asked for once it has come back.
    let after = bot.api.count("getUpdates");
    flush().await;
    assert_eq!(bot.api.count("getUpdates"), after);
}

#[tokio::test]
async fn an_update_that_is_neither_a_message_nor_a_press_is_nothing_to_do() {
    let bot = bot().await;

    bot.api
        .push(darkwire_channels::telegram::api::TelegramUpdate {
            update_id: 0,
            message: None,
            callback_query: None,
        });
    flush().await;

    assert!(bot.hub.messages().is_empty());
    assert_eq!(bot.manager.channels().len(), 1);
    bot.manager.stop().await;
}

#[tokio::test]
async fn a_press_with_no_message_behind_it_is_answered_and_dropped() {
    let bot = bot().await;
    let mut update = callback_update("t1", USER, None);
    if let Some(query) = update.callback_query.as_mut() {
        query.message = None;
    }

    bot.api.push(update);
    flush().await;

    assert_eq!(bot.api.count("answerCallbackQuery"), 1);
    assert!(!bot.api.bodies("answerCallbackQuery")[0].contains_key("text"));
    bot.manager.stop().await;
}

#[tokio::test]
async fn the_banner_names_the_bot_once_it_has_connected() {
    let api = FakeBotApi::new();
    api.reply(
        "getMe",
        CannedAnswer::ok(&json!({ "id": 1, "username": "ghost_test_bot" })),
    );
    let console = FakeConsole::new().expect("the stores open");
    let hub = ScriptedHub::silent();
    let manager = build(&api, &console, &hub, allowlisted(), None);
    manager.start().await.expect("it connects");

    // A command addressed to that name is ours to answer.
    api.push(message_update("/help@ghost_test_bot", USER, Some(GROUP)));
    flush().await;

    assert!(api.said("These are the commands"), "{:?}", api.texts());
    manager.stop().await;
}
