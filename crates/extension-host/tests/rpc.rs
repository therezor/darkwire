//! The wire: framing, id matching, both directions, and cancellation.
//!
//! Everything here speaks over `tokio::io::duplex`, so nothing spawns a process
//! or waits on a real timer. The point is that the framing is provable without
//! a child: `conformance.rs` proves the child, and this proves the protocol.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use darkwire_core::ErrorKind;
use darkwire_extension_host::{
    DarkwireInit, NoHostMethods, REQUEST_TIMEOUT, RpcClient, RpcError, RpcFailure, RpcHandler,
};
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio_util::sync::CancellationToken;

/// The other end of the pipe: a hand-driven peer, one line at a time.
struct Peer {
    lines: tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
    writer: WriteHalf<DuplexStream>,
}

impl Peer {
    /// The next frame this peer was sent.
    async fn next(&mut self) -> Value {
        let line = self
            .lines
            .next_line()
            .await
            .expect("the pipe is open")
            .expect("a frame arrives");
        serde_json::from_str(&line).expect("every frame is JSON")
    }

    /// The next frame that is a *request*, skipping notifications.
    async fn next_request(&mut self) -> Value {
        loop {
            let frame = self.next().await;
            if frame.get("id").is_some() {
                return frame;
            }
        }
    }

    async fn write(&mut self, frame: &Value) {
        let line = format!("{}\n", serde_json::to_string(frame).unwrap());
        self.writer.write_all(line.as_bytes()).await.unwrap();
    }

    async fn reply(&mut self, id: &Value, result: Value) {
        self.write(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
            .await;
    }

    async fn fail(&mut self, id: &Value, code: i64, message: &str) {
        self.write(&json!({
            "jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}
        }))
        .await;
    }
}

fn connect(handler: Arc<dyn RpcHandler>) -> (Arc<RpcClient>, Peer, CancellationToken) {
    let (ours, theirs) = tokio::io::duplex(64 * 1024);
    let (our_read, our_write) = tokio::io::split(ours);
    let (their_read, their_write) = tokio::io::split(theirs);
    let token = CancellationToken::new();
    let client = RpcClient::start(our_read, our_write, handler, &token);
    (
        client,
        Peer {
            lines: BufReader::new(their_read).lines(),
            writer: their_write,
        },
        token,
    )
}

fn init() -> DarkwireInit {
    DarkwireInit {
        extension_id: "hello".to_owned(),
        settings: serde_json::from_value(json!({"greeting": "Ahoy"})).unwrap(),
        data_dir: "/tmp/extension-data/hello".to_owned(),
        host_version: "0.8.1".to_owned(),
    }
}

#[tokio::test]
async fn the_handshake_is_mcp_plus_one_block_under_meta() {
    let (client, mut peer, _token) = connect(Arc::new(NoHostMethods));
    let handshake = tokio::spawn(async move { client.initialize(&init()).await });

    let frame = peer.next_request().await;
    assert_eq!(frame["method"], "initialize");
    assert_eq!(frame["jsonrpc"], "2.0");
    // The name is what an MCP server sees; everything DarkWire-specific is one
    // block below, where a server that never heard of us simply ignores it.
    assert_eq!(frame["params"]["clientInfo"]["name"], "darkwire");
    let ours = &frame["params"]["_meta"]["darkwire"];
    assert_eq!(ours["extensionId"], "hello");
    assert_eq!(ours["settings"]["greeting"], "Ahoy");
    assert_eq!(ours["dataDir"], "/tmp/extension-data/hello");
    assert_eq!(ours["hostVersion"], "0.8.1");

    peer.reply(&frame["id"], json!({"protocolVersion": "2025-06-18"}))
        .await;

    let result = handshake.await.unwrap().unwrap();
    assert_eq!(result.protocol_version, "2025-06-18");

    // MCP requires the notification after the reply, and a server may wait for
    // it before answering anything else.
    let after = peer.next().await;
    assert_eq!(after["method"], "notifications/initialized");
    assert!(after.get("id").is_none());
}

#[tokio::test]
async fn a_method_the_peer_does_not_implement_is_a_typed_failure() {
    let (client, mut peer, _token) = connect(Arc::new(NoHostMethods));
    let call = {
        let client = Arc::clone(&client);
        tokio::spawn(async move { client.request("darkwire/commands/list", json!({})).await })
    };

    let frame = peer.next_request().await;
    peer.fail(&frame["id"], -32601, "Method not found").await;

    let failure = call.await.unwrap().unwrap_err();
    // Read as a code, never as a message substring: it is the difference
    // between a warning on a row and a failed extension.
    assert!(failure.is_method_not_found());
}

#[tokio::test]
async fn replies_are_matched_to_their_own_call_out_of_order() {
    let (client, mut peer, _token) = connect(Arc::new(NoHostMethods));
    let first = {
        let client = Arc::clone(&client);
        tokio::spawn(async move { client.request("one", json!({})).await })
    };
    let first_frame = peer.next_request().await;
    let second = {
        let client = Arc::clone(&client);
        tokio::spawn(async move { client.request("two", json!({})).await })
    };
    let second_frame = peer.next_request().await;

    // Answered backwards, which is what a server with two workers does.
    peer.reply(&second_frame["id"], json!("second")).await;
    peer.reply(&first_frame["id"], json!("first")).await;

    assert_eq!(first.await.unwrap().unwrap(), json!("first"));
    assert_eq!(second.await.unwrap().unwrap(), json!("second"));
}

/// A host side that records what it was asked and answers one method.
struct RecordingHost {
    seen: Arc<Mutex<Vec<(String, Value)>>>,
}

impl RpcHandler for RecordingHost {
    fn request(
        &self,
        method: String,
        params: Value,
    ) -> BoxFuture<'_, std::result::Result<Value, RpcError>> {
        self.seen.lock().push((method.clone(), params));
        Box::pin(async move {
            if method == "darkwire/secret" {
                Ok(json!({"value": "shhh"}))
            } else {
                Err(RpcError::method_not_found(&method))
            }
        })
    }

    fn notify(&self, method: String, params: Value) {
        self.seen.lock().push((method, params));
    }
}

#[tokio::test]
async fn an_extension_can_ask_the_host_and_announce_to_it() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (_client, mut peer, _token) = connect(Arc::new(RecordingHost {
        seen: Arc::clone(&seen),
    }));

    peer.write(&json!({
        "jsonrpc": "2.0", "id": 7, "method": "darkwire/secret", "params": {}
    }))
    .await;
    let answer = peer.next().await;
    assert_eq!(answer["id"], 7);
    assert_eq!(answer["result"]["value"], "shhh");

    // A method the host does not serve is refused with the same code the
    // extension side uses, rather than met with silence.
    peer.write(&json!({"jsonrpc": "2.0", "id": 8, "method": "darkwire/vault/read"}))
        .await;
    let refusal = peer.next().await;
    assert_eq!(refusal["error"]["code"], -32601);

    peer.write(&json!({
        "jsonrpc": "2.0", "method": "darkwire/channels/publish", "params": {"text": "hi"}
    }))
    .await;
    tokio::task::yield_now().await;
    let recorded = seen.lock().clone();
    assert!(
        recorded
            .iter()
            .any(|(method, _)| method == "darkwire/channels/publish")
    );
}

#[tokio::test]
async fn cancelling_a_call_notifies_the_peer_and_stops_waiting() {
    let (client, mut peer, _token) = connect(Arc::new(NoHostMethods));
    let token = CancellationToken::new();
    let call = {
        let client = Arc::clone(&client);
        let token = token.clone();
        tokio::spawn(async move {
            client
                .request_cancellable("darkwire/commands/run", json!({"id": "slow"}), &token)
                .await
        })
    };

    let frame = peer.next_request().await;
    let id = frame["id"].clone();
    token.cancel();

    let failure = call.await.unwrap().unwrap_err();
    assert!(!failure.is_method_not_found());

    // MCP's own cancellation, naming the request that is being abandoned — the
    // peer may still answer, and the host has already stopped listening.
    let notification = peer.next().await;
    assert_eq!(notification["method"], "notifications/cancelled");
    assert_eq!(notification["params"]["requestId"], id);
}

#[tokio::test]
async fn a_closed_connection_fails_everything_still_waiting() {
    let (client, mut peer, _token) = connect(Arc::new(NoHostMethods));
    let call = {
        let client = Arc::clone(&client);
        tokio::spawn(async move { client.request("darkwire/context/static", json!({})).await })
    };
    peer.next_request().await;

    drop(peer);

    let failure = call.await.unwrap().unwrap_err();
    assert!(failure.message().contains("not answering"));
    // And a call made afterwards fails at once rather than hanging.
    assert!(client.request("anything", json!({})).await.is_err());
}

#[tokio::test]
async fn a_line_that_is_not_a_frame_does_not_take_the_connection_down() {
    let (client, mut peer, _token) = connect(Arc::new(NoHostMethods));
    // A child that printed a banner to stdout has broken the wire; it has not
    // earned an unloadable extension.
    peer.writer
        .write_all(b"listening on 3000\n{not json\n")
        .await
        .unwrap();

    let call = {
        let client = Arc::clone(&client);
        tokio::spawn(async move { client.request("tools/list", json!({})).await })
    };
    let frame = peer.next_request().await;
    peer.reply(&frame["id"], json!({"tools": []})).await;
    assert_eq!(call.await.unwrap().unwrap(), json!({"tools": []}));
    assert!(!client.is_closed());
}

#[tokio::test]
async fn a_request_with_a_string_id_is_answered_under_that_id() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (_client, mut peer, _token) = connect(Arc::new(RecordingHost {
        seen: Arc::clone(&seen),
    }));

    peer.write(&json!({
        "jsonrpc": "2.0", "id": "a1", "method": "darkwire/secret", "params": {}
    }))
    .await;
    let answer = peer.next().await;
    assert_eq!(answer["id"], "a1");
    assert_eq!(answer["result"]["value"], "shhh");
}

fn in_flight(client: &RpcClient) -> String {
    format!("{client:?}")
}

#[tokio::test(start_paused = true)]
async fn a_request_nobody_answers_times_out_and_leaves_nothing_waiting() {
    let (client, mut peer, _token) = connect(Arc::new(NoHostMethods));
    let call = {
        let client = Arc::clone(&client);
        tokio::spawn(async move { client.request("darkwire/channels/start", json!({})).await })
    };
    let frame = peer.next_request().await;
    assert!(in_flight(&client).contains("in_flight: 1"));

    tokio::time::advance(REQUEST_TIMEOUT).await;
    let failure = call.await.unwrap().unwrap_err();
    let RpcFailure::Transport(error) = failure else {
        panic!("a timeout is not the peer's answer");
    };
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert!(in_flight(&client).contains("in_flight: 0"));

    // The peer is told, so a well-behaved one stops working on it.
    let notification = peer.next().await;
    assert_eq!(notification["method"], "notifications/cancelled");
    assert_eq!(notification["params"]["requestId"], frame["id"]);
}

#[tokio::test(start_paused = true)]
async fn a_caller_that_stops_waiting_leaves_nothing_waiting() {
    let (client, mut peer, _token) = connect(Arc::new(NoHostMethods));
    let call = {
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                client.request("darkwire/context/runtime", json!({})),
            )
            .await
        })
    };
    peer.next_request().await;
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    assert!(call.await.unwrap().is_err());
    assert!(in_flight(&client).contains("in_flight: 0"));
}
