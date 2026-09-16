//! The one suite that runs the real client against a real MCP server.
//!
//! Everything else in this crate speaks `McpSession`, which is what keeps the
//! suite fast and keeps a subprocess out of CI — but it also means nothing else
//! would notice if this adapter stopped speaking the protocol. An in-memory
//! duplex pipe is how that gets checked without opening anything: the SDK's own
//! server on one end, and the same client the real connector builds on the
//! other.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;
use std::time::Duration;

use darkwire_core::ErrorKind;
use darkwire_mcp::{
    McpCallOptions, McpConnectContext, McpConnectionSpec, McpConnector, McpSession,
    McpSessionEvent, SdkConnector, SdkConnectorOptions, default_environment, resolve_spec,
};
use darkwire_protocol::json::Object;
use parking_lot::Mutex;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer, RunningService};
use rmcp::{ErrorData, ServerHandler, serve_server};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn spec() -> McpConnectionSpec {
    resolve_spec(
        "demo",
        &serde_json::from_value(json!({ "command": "irrelevant" })).unwrap(),
    )
    .unwrap()
}

fn schema(properties: Value) -> Arc<serde_json::Map<String, Value>> {
    let mut map = serde_json::Map::new();
    map.insert("type".to_owned(), json!("object"));
    map.insert("properties".to_owned(), properties);
    Arc::new(map)
}

/// The SDK's server half, advertising `repeat` and whatever a test adds.
#[derive(Clone)]
struct Demo {
    tools: Arc<Mutex<Vec<Tool>>>,
}

impl Demo {
    fn new() -> Demo {
        Demo {
            tools: Arc::new(Mutex::new(vec![Tool::new(
                "repeat",
                "Repeats a string.",
                schema(
                    json!({ "text": { "type": "string", "description": "The string to repeat." } }),
                ),
            )])),
        }
    }
}

impl ServerHandler for Demo {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .build();
        info.server_info = Implementation::new("demo-server", "2.1.0");
        info
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.lock().clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let result = match request.name.as_ref() {
            "repeat" => {
                let text = request
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                CallToolResult::success(vec![ContentBlock::text(text)])
            }
            "explode" => CallToolResult::error(vec![ContentBlock::text("boom")]),
            "slow" => {
                tokio::time::sleep(Duration::from_secs(2)).await;
                CallToolResult::success(vec![])
            }
            other => return Err(ErrorData::invalid_params(format!("no tool {other}"), None)),
        };
        Ok(CallToolResponse::Complete(result))
    }
}

/// A live server on one end of an in-memory pipe, and a session on the other.
struct Live {
    demo: Demo,
    running: RunningService<RoleServer, Demo>,
    session: Arc<dyn McpSession>,
}

/// Starts the server half and connects to it.
///
/// The server is spawned rather than awaited: `serve_server` completes only
/// once a client has initialised, so awaiting it before dialling would be a
/// deadlock between the two halves of the same handshake.
async fn live() -> Live {
    let (client_side, server_side) = tokio::io::duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client_side);
    let (server_read, server_write) = tokio::io::split(server_side);
    let demo = Demo::new();
    let serving = tokio::spawn(serve_server(demo.clone(), (server_read, server_write)));

    let pipe = Mutex::new(Some((client_read, client_write)));
    let connector = SdkConnector::new(SdkConnectorOptions {
        client_name: Some("darkwire-test".to_owned()),
        client_version: None,
        http: reqwest::Client::new(),
        pipe: Some(Arc::new(move || {
            pipe.lock()
                .take()
                .map(|(read, write)| {
                    (
                        Box::new(read) as darkwire_mcp::sdk_connector::BoxRead,
                        Box::new(write) as darkwire_mcp::sdk_connector::BoxWrite,
                    )
                })
                .ok_or_else(|| std::io::Error::other("the pipe was already used"))
        })),
    });
    let session = connector
        .connect(spec(), McpConnectContext::bare(CancellationToken::new()))
        .await
        .expect("the client half initialises");
    let running = serving
        .await
        .expect("the server task")
        .expect("the server half initialises");
    Live {
        demo,
        running,
        session,
    }
}

fn options() -> McpCallOptions {
    McpCallOptions {
        token: CancellationToken::new(),
        timeout_ms: 0,
    }
}

fn args(value: Value) -> Object {
    match value {
        Value::Object(map) => map.into_iter().collect(),
        _ => Object::new(),
    }
}

#[tokio::test]
async fn completes_the_handshake_and_reports_who_answered() {
    let Live {
        running, session, ..
    } = live().await;
    assert_eq!(session.server_name(), "demo-server");
    assert_eq!(session.server_version(), "2.1.0");
    assert!(session.warnings().is_empty());
    session.close().await;
    let _ = running.cancel().await;
}

#[tokio::test]
async fn reads_the_advertised_tools_as_descriptors_the_bridge_understands() {
    let Live {
        running, session, ..
    } = live().await;
    let tools = session.list_tools(CancellationToken::new()).await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "repeat");
    assert_eq!(tools[0].description.as_deref(), Some("Repeats a string."));
    // The shape `normalise_schema` is handed. If the SDK ever stopped supplying
    // raw JSON Schema here, every bridged tool would silently lose its
    // arguments, and this is the assertion that would say so.
    assert_eq!(tools[0].input_schema["type"], "object");
    assert_eq!(
        tools[0].input_schema["properties"]["text"]["type"],
        "string"
    );
    session.close().await;
    let _ = running.cancel().await;
}

#[tokio::test]
async fn calls_a_tool_and_returns_its_content_parts() {
    let Live {
        running, session, ..
    } = live().await;
    let result = session
        .call_tool("repeat", args(json!({ "text": "hello" })), options())
        .await
        .unwrap();
    assert_eq!(result.content.len(), 1);
    assert_eq!(result.content[0].kind, "text");
    assert_eq!(result.content[0].text.as_deref(), Some("hello"));
    assert_eq!(result.is_error, Some(false));
    session.close().await;
    let _ = running.cancel().await;
}

#[tokio::test]
async fn calls_a_tool_under_a_per_call_cap() {
    let Live {
        running, session, ..
    } = live().await;
    let result = session
        .call_tool(
            "repeat",
            args(json!({ "text": "capped" })),
            McpCallOptions {
                token: CancellationToken::new(),
                timeout_ms: 5_000,
            },
        )
        .await
        .unwrap();
    assert_eq!(result.content[0].text.as_deref(), Some("capped"));
    session.close().await;
    let _ = running.cancel().await;
}

#[tokio::test]
async fn times_out_a_server_that_stopped_answering() {
    let Live {
        running, session, ..
    } = live().await;
    let error = session
        .call_tool(
            "slow",
            Object::new(),
            McpCallOptions {
                token: CancellationToken::new(),
                timeout_ms: 50,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    session.close().await;
    let _ = running.cancel().await;
}

#[tokio::test]
async fn a_cancelled_token_abandons_the_call() {
    let Live {
        running, session, ..
    } = live().await;
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    });
    let error = session
        .call_tool(
            "slow",
            Object::new(),
            McpCallOptions {
                token,
                timeout_ms: 5_000,
            },
        )
        .await
        .unwrap_err();
    assert!(error.is_aborted());
    session.close().await;
    let _ = running.cancel().await;
}

#[tokio::test]
async fn reports_a_tool_that_failed_as_a_result_not_an_error() {
    let Live {
        demo,
        running,
        session,
    } = live().await;
    demo.tools
        .lock()
        .push(Tool::new("explode", "Always fails.", schema(json!({}))));
    let result = session
        .call_tool("explode", Object::new(), options())
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));
    session.close().await;
    let _ = running.cancel().await;
}

#[tokio::test]
async fn a_tool_the_server_refuses_is_a_tool_error() {
    let Live {
        running, session, ..
    } = live().await;
    let error = session
        .call_tool("missing", Object::new(), options())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Tool);
    session.close().await;
    let _ = running.cancel().await;
}

#[tokio::test]
async fn fires_the_tool_list_event_when_the_server_says_the_list_moved() {
    let Live {
        demo,
        running,
        session,
    } = live().await;
    let mut events = session.subscribe();

    demo.tools.lock().push(Tool::new(
        "reverse",
        "Reverses a string.",
        schema(json!({})),
    ));
    running.peer().notify_tool_list_changed().await.unwrap();

    let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("an event in time")
        .unwrap();
    assert!(matches!(event, McpSessionEvent::ToolListChanged));
    let mut names: Vec<String> = session
        .list_tools(CancellationToken::new())
        .await
        .unwrap()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    names.sort();
    assert_eq!(names, ["repeat", "reverse"]);
    session.close().await;
    let _ = running.cancel().await;
}

#[tokio::test]
async fn tells_its_listeners_when_the_transport_goes_away() {
    let Live {
        running, session, ..
    } = live().await;
    let mut events = session.subscribe();

    let _ = running.cancel().await;
    let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("a close in time")
        .unwrap();
    assert!(matches!(event, McpSessionEvent::Closed(_)));
}

#[tokio::test]
async fn does_not_report_its_own_close_as_a_drop() {
    let Live {
        running, session, ..
    } = live().await;
    let mut events = session.subscribe();

    session.close().await;
    // A connection told to shut down must not read its own teardown as a drop
    // and arm a reconnect: nothing arrives but the channel closing.
    let outcome = tokio::time::timeout(Duration::from_millis(300), events.recv()).await;
    assert!(
        matches!(outcome, Err(_) | Ok(Err(_))),
        "unexpected event after close: {outcome:?}"
    );
    let _ = running.cancel().await;
}

#[tokio::test]
async fn reports_a_refused_connection_as_a_network_error() {
    let connector = SdkConnector::new(SdkConnectorOptions {
        pipe: Some(Arc::new(|| Err(std::io::Error::other("ECONNREFUSED")))),
        ..SdkConnectorOptions::default()
    });
    let error = connector
        .connect(spec(), McpConnectContext::bare(CancellationToken::new()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(error.message.contains("ECONNREFUSED"));
}

#[tokio::test]
async fn a_cancelled_connect_is_aborted() {
    let (client_side, _server_side) = tokio::io::duplex(1024);
    let (read, write) = tokio::io::split(client_side);
    let pipe = Mutex::new(Some((read, write)));
    let connector = SdkConnector::new(SdkConnectorOptions {
        pipe: Some(Arc::new(move || {
            pipe.lock()
                .take()
                .map(|(read, write)| {
                    (
                        Box::new(read) as darkwire_mcp::sdk_connector::BoxRead,
                        Box::new(write) as darkwire_mcp::sdk_connector::BoxWrite,
                    )
                })
                .ok_or_else(|| std::io::Error::other("used"))
        })),
        ..SdkConnectorOptions::default()
    });
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    });
    // Nobody answers on the other end; the token is the only way out.
    let error = connector
        .connect(spec(), McpConnectContext::bare(token))
        .await
        .unwrap_err();
    assert!(
        error.is_aborted() || error.kind == ErrorKind::Network,
        "{error:?}"
    );
}

#[tokio::test]
async fn a_stdio_command_that_does_not_exist_is_a_network_error_naming_it() {
    let connector = SdkConnector::new(SdkConnectorOptions::default());
    let spec = resolve_spec(
        "ghost",
        &serde_json::from_value(json!({ "command": "/nonexistent/darkwire-mcp-test-binary" }))
            .unwrap(),
    )
    .unwrap();
    let error = connector
        .connect(spec, McpConnectContext::bare(CancellationToken::new()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(error.message.contains("darkwire-mcp-test-binary"));
}

#[tokio::test]
async fn a_header_that_is_not_a_header_is_a_config_error() {
    let connector = SdkConnector::new(SdkConnectorOptions::default());
    let spec = resolve_spec(
        "remote",
        &serde_json::from_value(json!({
            "url": "http://127.0.0.1:9/mcp",
            "headers": { "bad header": "x" }
        }))
        .unwrap(),
    )
    .unwrap();
    let error = connector
        .connect(spec, McpConnectContext::bare(CancellationToken::new()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
}

#[tokio::test]
async fn an_sse_server_that_cannot_be_reached_says_why() {
    let connector = SdkConnector::new(SdkConnectorOptions::default());
    let spec = resolve_spec(
        "legacy",
        &serde_json::from_value(json!({ "type": "sse", "url": "http://127.0.0.1:9/sse" })).unwrap(),
    )
    .unwrap();
    let error = connector
        .connect(spec, McpConnectContext::bare(CancellationToken::new()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(
        error.message.contains("dialled as Streamable HTTP"),
        "the operator is left guessing: {}",
        error.message
    );
}

#[test]
fn the_default_environment_is_the_minimal_inherited_set() {
    let env = default_environment();
    for name in env.keys() {
        assert!(
            darkwire_mcp::DEFAULT_INHERITED_ENV_VARS.contains(&name.as_str()),
            "{name} leaked into the child environment"
        );
    }
    assert!(env.get("PATH").is_some_and(|path| !path.is_empty()));
}
