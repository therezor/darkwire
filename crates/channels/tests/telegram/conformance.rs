//! The shipped channel, against the suite written for channels that are not.
//!
//! This is the property the crate header rests on: the built-in consumes
//! exactly the factory contract an extension would, so it is held to the
//! contract by the same test everyone else is. The contract cannot rot into
//! "whatever Telegram needed", because Telegram is not allowed to be the only
//! thing that passes.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use darkwire_channels::channel::BoxFuture;
use darkwire_channels::telegram::api::HttpClient;
use darkwire_channels::telegram::channel::{TelegramChannelOptions, telegram_channel};
use darkwire_channels::telegram::console::TelegramConsole;
use darkwire_channels::testkit::{
    ChannelConformanceOptions, ChannelProbe, ChannelUnderTest, channel_conformance,
};
use serde_json::{Map, Value, json};

use crate::console_double::FakeConsole;
use crate::fake_bot_api::{FakeBotApi, message_update};

const USER: i64 = 4471;

/// The suite's view of a Telegram bot: a user typing, and what the bot posted.
struct TelegramProbe {
    api: Arc<FakeBotApi>,
    /// Held, never read: dropping it would take the stores the channel reads
    /// out from under the scenario still running against them.
    #[allow(
        dead_code,
        reason = "kept alive for the channel, not read by the probe"
    )]
    console: Arc<FakeConsole>,
}

impl ChannelProbe for TelegramProbe {
    fn receive<'a>(&'a self, text: &'a str) -> BoxFuture<'a, ()> {
        self.api.push(message_update(text, USER, None));
        Box::pin(std::future::ready(()))
    }

    fn sent(&self) -> Vec<String> {
        self.api.texts()
    }
}

fn under_test() -> ChannelUnderTest {
    let api = FakeBotApi::new();
    let console = FakeConsole::new().expect("the stores open");
    let ids = Arc::new(AtomicUsize::new(0));
    let factory = telegram_channel(TelegramChannelOptions {
        token: "12345:secret".to_owned(),
        console: Arc::clone(&console) as Arc<dyn TelegramConsole>,
        new_id: Arc::new(move || format!("id{}", ids.fetch_add(1, Ordering::SeqCst))),
        http: Some(Arc::clone(&api) as Arc<dyn HttpClient>),
        id: "telegram".to_owned(),
    });
    ChannelUnderTest {
        factory,
        probe: Arc::new(TelegramProbe { api, console }),
    }
}

fn settings() -> Map<String, Value> {
    json!({ "allowlist": [USER.to_string()], "pollTimeoutSec": 1 })
        .as_object()
        .cloned()
        .expect("an object")
}

#[tokio::test]
async fn the_shipped_channel_satisfies_the_same_contract_an_extension_would() {
    channel_conformance(&ChannelConformanceOptions {
        make: Arc::new(under_test),
        settings: settings(),
    })
    .await;
}
