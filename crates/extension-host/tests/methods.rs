//! The method set: what an extension may say to the host, and how what it
//! answers becomes a prompt section.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use darkwire_channels::{ChannelContext, ChannelControlFrame};
use darkwire_core::SystemClock;
use darkwire_core::message_bus::PublishResult;
use darkwire_extension_host::methods::{
    CHANNELS_CONTROL, CHANNELS_PUBLISH, ContextSections, HostMethods, SECRET, control_frame_of,
};
use darkwire_extension_host::rpc::RpcHandler;
use darkwire_protocol::ClientMessage;
use parking_lot::Mutex;
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn sections(value: serde_json::Value) -> ContextSections {
    serde_json::from_value(value).unwrap()
}

#[test]
fn sections_render_as_headed_blocks() {
    let rendered = sections(json!({"sections": [
        {"title": "Hello", "body": "One paragraph."},
        {"title": "", "body": "Unheaded."},
    ]}))
    .render()
    .unwrap();
    assert_eq!(rendered, "## Hello\n\nOne paragraph.\n\nUnheaded.");
}

#[test]
fn a_heading_with_nothing_under_it_places_nothing() {
    // A model reads an empty section as one it failed to understand, which is
    // worse than no section at all.
    assert_eq!(
        sections(json!({"sections": [{"title": "Hello", "body": "  "}]})).render(),
        None
    );
    assert_eq!(sections(json!({"sections": []})).render(), None);
    assert_eq!(sections(json!({})).render(), None);
}

#[test]
fn the_five_frames_a_channel_may_send_are_the_five_it_may_send() {
    let approve: ClientMessage =
        serde_json::from_value(json!({"type": "tool.approve", "callId": "c1", "approved": true}))
            .unwrap();
    assert!(matches!(
        control_frame_of(approve),
        Ok(ChannelControlFrame::ToolApprove(_))
    ));

    let stop: ClientMessage =
        serde_json::from_value(json!({"type": "turn.stop", "sessionKey": "s1"})).unwrap();
    assert!(matches!(
        control_frame_of(stop),
        Ok(ChannelControlFrame::StopTurn(_))
    ));

    // The three that move the *connection* are refused with a sentence naming
    // the frame: a channel changes conversation by publishing a different
    // session key, never by moving where its events arrive.
    let switch: ClientMessage =
        serde_json::from_value(json!({"type": "session.switch", "sessionKey": "s2"})).unwrap();
    let refusal = control_frame_of(switch).unwrap_err();
    assert!(refusal.contains("session.switch"), "{refusal}");

    let ping: ClientMessage = serde_json::from_value(json!({"type": "ping"})).unwrap();
    assert!(control_frame_of(ping).is_err());

    // Saving a rule writes the agent's settings, which is the operator's.
    let with_rule: ClientMessage = serde_json::from_value(json!({
        "type": "tool.approve", "callId": "c1", "approved": true,
        "rule": {"action": "allow", "argv": ["ls"]},
    }))
    .unwrap();
    let refusal = control_frame_of(with_rule).unwrap_err();
    assert!(refusal.contains("command rule"), "{refusal}");
}

#[tokio::test]
async fn the_secret_method_takes_no_arguments_and_answers_this_extensions_own() {
    let host = HostMethods::new("slack", Some(Arc::new(|| Some("xoxb-secret".to_owned()))));
    // No parameters at all: there is no shape of this call that names another
    // extension's credential.
    let answer = host.request(SECRET.to_owned(), json!({})).await.unwrap();
    assert_eq!(answer["value"], "xoxb-secret");

    let none = HostMethods::new("slack", None);
    let answer = none.request(SECRET.to_owned(), json!({})).await.unwrap();
    assert!(answer["value"].is_null());
}

#[tokio::test]
async fn every_other_host_method_is_method_not_found() {
    let host = HostMethods::new("slack", None);
    for method in ["darkwire/vault/read", "darkwire/config/patch", "tools/call"] {
        let error = host
            .request(method.to_owned(), json!({}))
            .await
            .unwrap_err();
        assert_eq!(error.code, -32601);
    }
}

/// What a recording channel context saw.
type Seen = Arc<Mutex<Vec<String>>>;

/// A channel context that records what reached the bus through it.
fn recording(id: &str) -> (ChannelContext, Seen, Seen) {
    let published = Arc::new(Mutex::new(Vec::new()));
    let controlled = Arc::new(Mutex::new(Vec::new()));
    let into_publish = Arc::clone(&published);
    let into_control = Arc::clone(&controlled);
    let context = ChannelContext {
        id: id.to_owned(),
        settings: serde_json::Map::new(),
        clock: Arc::new(SystemClock),
        token: CancellationToken::new(),
        publish: Arc::new(move |message| {
            into_publish.lock().push(message.session_key.clone());
            PublishResult::Accepted {
                id: "m1".to_owned(),
            }
        }),
        control: Arc::new(move |command| {
            into_control.lock().push(command.frame.tag().to_owned());
        }),
    };
    (context, published, controlled)
}

#[tokio::test]
async fn a_published_message_reaches_the_channel_it_names() {
    let host = HostMethods::new("slack", None);
    let (context, published, _) = recording("slack");
    host.bind_channel(context);

    host.notify(
        CHANNELS_PUBLISH.to_owned(),
        json!({"channelId": "slack", "sessionKey": "s1", "senderId": "u1", "content": []}),
    );
    assert_eq!(published.lock().clone(), vec!["s1".to_owned()]);

    // A channel it does not have running is dropped, not routed anywhere: the
    // id is how the bus decides whose message this is.
    host.notify(
        CHANNELS_PUBLISH.to_owned(),
        json!({"channelId": "someone-else", "sessionKey": "s2", "content": []}),
    );
    assert_eq!(published.lock().len(), 1);

    // And so is one the host cannot even read.
    host.notify(CHANNELS_PUBLISH.to_owned(), json!({"nope": true}));
    assert_eq!(published.lock().len(), 1);
}

#[tokio::test]
async fn a_control_frame_reaches_the_channels_own_connection() {
    let host = HostMethods::new("slack", None);
    let (context, _, controlled) = recording("slack");
    host.bind_channel(context);

    host.notify(
        CHANNELS_CONTROL.to_owned(),
        json!({
            "channelId": "slack",
            "sessionKey": "s1",
            "frame": {"type": "turn.stop", "sessionKey": "s1"},
        }),
    );
    assert_eq!(controlled.lock().clone(), vec!["turn.stop".to_owned()]);

    // A frame a channel may not send is dropped with a log line, not delivered.
    host.notify(
        CHANNELS_CONTROL.to_owned(),
        json!({
            "channelId": "slack",
            "sessionKey": "s1",
            "frame": {"type": "session.new"},
        }),
    );
    assert_eq!(controlled.lock().len(), 1);

    // Unbinding is what an unload does, and it stops delivery at once.
    host.clear_channels();
    host.notify(
        CHANNELS_CONTROL.to_owned(),
        json!({
            "channelId": "slack",
            "sessionKey": "s1",
            "frame": {"type": "turn.stop", "sessionKey": "s1"},
        }),
    );
    assert_eq!(controlled.lock().len(), 1);
}

#[tokio::test]
async fn a_log_notification_is_swallowed_rather_than_answered() {
    let host = HostMethods::new("slack", None);
    for level in ["debug", "info", "warning", "error", "made up"] {
        host.notify(
            "notifications/message".to_owned(),
            json!({"level": level, "data": "something happened"}),
        );
    }
    // Structured data, not a string, is still logged rather than dropped.
    host.notify(
        "notifications/message".to_owned(),
        json!({"level": "info", "data": {"at": 1}}),
    );
    // And anything else is ignored without complaint.
    host.notify("notifications/progress".to_owned(), json!({}));
}
