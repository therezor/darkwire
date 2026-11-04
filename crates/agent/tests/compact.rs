//! A conversation summarised into a new session that opens with the summary.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "a fixture that cannot be built is a failing test either way"
)]

use crate::common;

use common::harness::{Harness, Setup};
use darkwire_agent::testkit::{ScriptedTurn, tool_call};
use darkwire_agent::{
    COMPACTED_FROM_KEY, CompactRequest, SUMMARY_PROMPT, SUMMARY_REQUEST, continued_title,
};
use darkwire_core::messages::{AssistantOptions, assistant_message, text_of, user_message};
use darkwire_core::session_store::{AppendOptions, CreateSession};
use darkwire_protocol::{ChatMessage, ContentPart, FilePart, FileTag};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn request(instructions: Option<&str>) -> CompactRequest {
    CompactRequest {
        session_key: "web:old".to_owned(),
        new_key: "web:new".to_owned(),
        instructions: instructions.map(str::to_owned),
    }
}

#[tokio::test]
async fn the_new_session_opens_with_the_request_and_the_summary() {
    let harness = Harness::build(Setup::with(vec![
        ScriptedTurn::text("the first answer"),
        ScriptedTurn::text("what happened so far"),
    ]));
    let (_, ended) = harness.say("web:old", "hello").await;
    ended.unwrap();

    let created = harness
        .agent_loop
        .compact(&request(Some("keep the names")), &CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(created.key, "web:new");
    assert_eq!(created.metadata[COMPACTED_FROM_KEY], json!("web:old"));
    let opened: Vec<String> = harness.stored("web:new").iter().map(text_of).collect();
    assert_eq!(
        opened,
        [
            format!("{SUMMARY_REQUEST}\n\nkeep the names"),
            "what happened so far".to_owned()
        ]
    );
    // The old conversation is left as it was.
    assert_eq!(harness.stored("web:old").len(), 2);

    let sent = harness.provider.requests().pop().unwrap();
    assert!(sent.tools.is_empty());
    assert_eq!(text_of(&sent.messages[0]), SUMMARY_PROMPT);
    assert_eq!(
        text_of(sent.messages.last().unwrap()),
        format!("{SUMMARY_REQUEST}\n\nkeep the names")
    );
}

#[tokio::test]
async fn an_empty_summary_writes_nothing() {
    let harness = Harness::build(Setup::with(vec![
        ScriptedTurn::text("the first answer"),
        ScriptedTurn::text("  "),
    ]));
    harness.say("web:old", "hello").await.1.unwrap();

    let refused = harness
        .agent_loop
        .compact(&request(None), &CancellationToken::new())
        .await;

    assert!(refused.is_err());
    assert!(harness.store.get_session("web:new").unwrap().is_none());
}

#[tokio::test]
async fn a_conversation_with_nothing_in_it_is_refused() {
    let harness = Harness::simple();
    harness
        .store
        .ensure_session("web:old", CreateSession::default())
        .unwrap();

    let refused = harness
        .agent_loop
        .compact(&request(None), &CancellationToken::new())
        .await;

    assert!(refused.is_err());
    assert!(harness.provider.requests().is_empty());
}

#[tokio::test]
async fn an_open_tool_call_and_an_attachment_are_not_sent_as_they_were() {
    let harness = Harness::build(Setup::with(vec![ScriptedTurn::text("summary")]));
    harness
        .store
        .ensure_session("web:old", CreateSession::default())
        .unwrap();
    let mut asked = user_message("look at this");
    asked.content.push(ContentPart::File(FilePart {
        tag: FileTag,
        mime_type: "image/png".to_owned(),
        path: "uploads/shot.png".to_owned(),
        name: Some("shot.png".to_owned()),
        size_bytes: None,
    }));
    let stopped = assistant_message(
        "",
        AssistantOptions {
            tool_calls: vec![tool_call("c1", "read", &json!({"path": "a"}))],
            ..AssistantOptions::default()
        },
    );
    harness
        .store
        .append_many(
            "web:old",
            vec![ChatMessage::User(asked), ChatMessage::Assistant(stopped)],
            &AppendOptions::default(),
        )
        .unwrap();

    harness
        .agent_loop
        .compact(&request(None), &CancellationToken::new())
        .await
        .unwrap();

    let sent = harness.provider.requests().pop().unwrap();
    let history = &sent.messages[1..sent.messages.len() - 1];
    assert_eq!(history.len(), 1, "the open call was dropped");
    assert_eq!(text_of(&history[0]), "look at this\n[attachment: shot.png]");
}

#[test]
fn a_continued_title_says_so_once() {
    assert_eq!(continued_title("Trip plans"), "Trip plans (continued)");
    assert_eq!(
        continued_title("Trip plans (continued)"),
        "Trip plans (continued)"
    );
    assert_eq!(continued_title(""), "Conversation (continued)");
}
