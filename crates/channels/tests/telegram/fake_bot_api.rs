//! Telegram, as a queue of canned answers.
//!
//! Not part of the testkit: it lives beside the tests that use it because
//! nothing outside this crate has a Telegram channel to drive. What it replaces
//! is the network — every test in `tests/telegram/` runs against this, so the
//! suite opens no socket and the paths that only a broken API produces (a 429
//! with a `retry_after`, a 409, a body that is not JSON) are reachable at all.
//!
//! `getUpdates` is the one method with a life of its own: it answers when a test
//! pushes an update and otherwise parks, which is what lets the poll loop be
//! driven a step at a time instead of raced against a timer.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use darkwire_channels::channel::BoxFuture;
use darkwire_channels::telegram::api::{
    HttpClient, HttpResponse, TelegramChat, TelegramMessage, TelegramMessageEntity, TelegramUpdate,
};
use darkwire_core::{ErrorKind, WireError};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// One recorded call: the method and the body it was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCall {
    /// The Bot API method, taken off the end of the URL.
    pub method: String,
    /// The JSON body it was posted.
    pub body: Map<String, Value>,
}

/// What a canned answer looks like. Either an envelope or a transport failure.
#[derive(Debug, Clone, Default)]
pub struct CannedAnswer {
    /// The HTTP status. Defaults to 200.
    pub status: Option<u16>,
    /// The body, as Telegram would send it.
    pub body: Option<Value>,
    /// A body that is not JSON at all — a proxy's error page.
    pub raw: Option<String>,
    /// A network failure: the request never got an answer.
    pub throws: Option<String>,
}

impl CannedAnswer {
    /// A successful envelope wrapping `result`.
    pub fn ok(result: &Value) -> CannedAnswer {
        CannedAnswer {
            status: Some(200),
            body: Some(json!({ "ok": true, "result": result })),
            ..CannedAnswer::default()
        }
    }
}

#[derive(Default)]
struct FakeState {
    calls: Vec<RecordedCall>,
    queued: HashMap<String, VecDeque<CannedAnswer>>,
    pending: Vec<TelegramUpdate>,
    next_message_id: i64,
    next_update_id: i64,
}

/// The Bot API, scripted.
#[derive(Default)]
pub struct FakeBotApi {
    state: Mutex<FakeState>,
    wake: Notify,
}

impl FakeBotApi {
    /// An API that answers every method with its default.
    pub fn new() -> Arc<FakeBotApi> {
        Arc::new(FakeBotApi {
            state: Mutex::new(FakeState {
                next_message_id: 100,
                ..FakeState::default()
            }),
            wake: Notify::new(),
        })
    }

    /// Queues one answer for the next call to `method`.
    ///
    /// The last queued answer stays, so a test that wants "and 429 for ever"
    /// queues it once.
    pub fn reply(&self, method: &str, answer: CannedAnswer) -> &FakeBotApi {
        self.state
            .lock()
            .queued
            .entry(method.to_owned())
            .or_default()
            .push_back(answer);
        self
    }

    /// Queues a Telegram-shaped failure.
    pub fn fail(&self, method: &str, code: u16, description: &str) -> &FakeBotApi {
        self.fail_with(method, code, description, None)
    }

    /// Queues a rate limit carrying its own `retry_after`.
    pub fn rate_limit(&self, method: &str, retry_after_sec: u64) -> &FakeBotApi {
        self.fail_with(method, 429, "Too Many Requests", Some(retry_after_sec))
    }

    fn fail_with(
        &self,
        method: &str,
        code: u16,
        description: &str,
        retry_after_sec: Option<u64>,
    ) -> &FakeBotApi {
        let mut body = json!({
            "ok": false,
            "error_code": code,
            "description": description,
        });
        if let Some(retry_after) = retry_after_sec
            && let Some(object) = body.as_object_mut()
        {
            object.insert(
                "parameters".to_owned(),
                json!({ "retry_after": retry_after }),
            );
        }
        self.reply(
            method,
            CannedAnswer {
                status: Some(code),
                body: Some(body),
                ..CannedAnswer::default()
            },
        )
    }

    /// A user typing, or a button being pressed. Wakes a parked `getUpdates`.
    pub fn push(&self, update: TelegramUpdate) {
        {
            let mut state = self.state.lock();
            let mut update = update;
            if update.update_id == 0 {
                state.next_update_id += 1;
                update.update_id = state.next_update_id;
            }
            state.pending.push(update);
        }
        self.wake.notify_waiters();
    }

    /// Every call made, oldest first.
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.state.lock().calls.clone()
    }

    /// Every call to one method, oldest first.
    pub fn bodies(&self, method: &str) -> Vec<Map<String, Value>> {
        self.state
            .lock()
            .calls
            .iter()
            .filter(|call| call.method == method)
            .map(|call| call.body.clone())
            .collect()
    }

    /// How many times one method was called.
    pub fn count(&self, method: &str) -> usize {
        self.bodies(method).len()
    }

    /// The `text` of everything the bot posted or edited, oldest first.
    pub fn texts(&self) -> Vec<String> {
        self.state
            .lock()
            .calls
            .iter()
            .filter(|call| call.method == "sendMessage" || call.method == "editMessageText")
            .map(|call| {
                call.body
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    /// Whether any posted or edited message said something containing `needle`.
    pub fn said(&self, needle: &str) -> bool {
        self.texts().iter().any(|text| text.contains(needle))
    }

    fn answer_for(&self, method: &str) -> Option<CannedAnswer> {
        let mut state = self.state.lock();
        let queue = state.queued.get_mut(method)?;
        if queue.len() > 1 {
            queue.pop_front()
        } else {
            queue.front().cloned()
        }
    }

    fn default_answer(&self, method: &str) -> CannedAnswer {
        match method {
            "getMe" => CannedAnswer::ok(&json!({ "id": 1, "username": "ghost_test_bot" })),
            "sendMessage" | "editMessageText" => {
                let mut state = self.state.lock();
                state.next_message_id += 1;
                CannedAnswer::ok(&json!({
                    "message_id": state.next_message_id,
                    "chat": { "id": 1, "type": "private" },
                }))
            }
            _ => CannedAnswer::ok(&json!(true)),
        }
    }

    /// Whatever has arrived, or a park until something does.
    ///
    /// The park is what makes the poll loop testable: without it `getUpdates`
    /// answers an empty list immediately and the loop spins as fast as the
    /// runtime allows, burning the test's timeout instead of waiting like a real
    /// long poll.
    ///
    /// A `timeout` of 0 is Telegram's short poll, and answers at once.
    async fn drain(
        &self,
        token: &CancellationToken,
        short: bool,
    ) -> Result<Vec<TelegramUpdate>, WireError> {
        loop {
            {
                let mut state = self.state.lock();
                if !state.pending.is_empty() || short {
                    return Ok(std::mem::take(&mut state.pending));
                }
            }
            if token.is_cancelled() {
                return Err(WireError::aborted("Telegram request"));
            }
            // Registered before the emptiness is re-checked above on the next
            // turn of the loop, so a push landing between the two is not missed.
            let waiting = self.wake.notified();
            tokio::select! {
                () = token.cancelled() => return Err(WireError::aborted("Telegram request")),
                () = waiting => {}
            }
        }
    }
}

impl HttpClient for FakeBotApi {
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        body: String,
        token: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<HttpResponse, WireError>> {
        Box::pin(async move {
            // A real call is not synchronous, and a poll loop retrying against a
            // purely synchronous fake would starve the runtime and hang the test
            // rather than failing it.
            tokio::task::yield_now().await;
            if token.is_cancelled() {
                return Err(WireError::aborted("Telegram request"));
            }

            let method = url.rsplit('/').next().unwrap_or_default().to_owned();
            let parsed: Map<String, Value> = serde_json::from_str(&body).unwrap_or_default();
            let short = parsed.get("timeout") == Some(&json!(0));
            self.state.lock().calls.push(RecordedCall {
                method: method.clone(),
                body: parsed,
            });

            let answer = match self.answer_for(&method) {
                Some(answer) => answer,
                None if method == "getUpdates" => {
                    CannedAnswer::ok(&json!(self.drain(token, short).await?))
                }
                None => self.default_answer(&method),
            };

            if let Some(failure) = answer.throws {
                return Err(WireError::new(ErrorKind::Network, failure));
            }
            Ok(HttpResponse {
                status: answer.status.unwrap_or(200),
                body: answer.raw.unwrap_or_else(|| {
                    answer
                        .body
                        .map_or_else(|| "null".to_owned(), |body| body.to_string())
                }),
            })
        })
    }
}

/// A message update, with the boilerplate a private chat always carries.
pub fn message_update(text: &str, user_id: i64, chat_id: Option<i64>) -> TelegramUpdate {
    let chat_id = chat_id.unwrap_or(user_id);
    let entities = if text.starts_with('/') {
        let word = text.split(' ').next().unwrap_or_default();
        vec![TelegramMessageEntity {
            kind: "bot_command".to_owned(),
            offset: 0,
            length: u32::try_from(word.encode_utf16().count()).unwrap_or(0),
        }]
    } else {
        Vec::new()
    };
    TelegramUpdate {
        update_id: 0,
        message: Some(TelegramMessage {
            message_id: 1,
            chat: TelegramChat {
                id: chat_id,
                kind: if chat_id == user_id {
                    "private".to_owned()
                } else {
                    "supergroup".to_owned()
                },
            },
            from: Some(darkwire_channels::telegram::api::TelegramUser {
                id: user_id,
                username: Some("tester".to_owned()),
            }),
            text: Some(text.to_owned()),
            entities,
        }),
        callback_query: None,
    }
}

/// A button press.
pub fn callback_update(data: &str, user_id: i64, chat_id: Option<i64>) -> TelegramUpdate {
    let chat_id = chat_id.unwrap_or(user_id);
    TelegramUpdate {
        update_id: 0,
        message: None,
        callback_query: Some(darkwire_channels::telegram::api::TelegramCallbackQuery {
            id: "cbq-1".to_owned(),
            from: darkwire_channels::telegram::api::TelegramUser {
                id: user_id,
                username: Some("tester".to_owned()),
            },
            data: Some(data.to_owned()),
            message: Some(TelegramMessage {
                message_id: 100,
                chat: TelegramChat {
                    id: chat_id,
                    kind: if chat_id == user_id {
                        "private".to_owned()
                    } else {
                        "supergroup".to_owned()
                    },
                },
                from: None,
                text: None,
                entities: Vec::new(),
            }),
        }),
    }
}
