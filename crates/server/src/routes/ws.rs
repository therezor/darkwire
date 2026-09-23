//! The socket, bound to the hub.
//!
//! There is almost nothing here, and that is the design working: [`SessionHub`]
//! is transport-agnostic — it hands back a [`HubClient`] to push inbound frames
//! into and an [`OutboundStream`] to drain — so binding it to a WebSocket is one
//! loop. Everything a client can do arrives as a frame the hub parses, and
//! everything the server says goes out through the one stream. A channel binds
//! the same pair without a socket, and gets the same queueing, the same replay
//! and the same approval gate.
//!
//! The four decisions worth stating:
//!
//!  - **A browser may only open it from the page this server served.** A
//!    WebSocket is not subject to the same-origin policy, so without this any
//!    page the operator visits could open one to a loopback install with
//!    authentication off, send a message, and approve its own tool calls. A
//!    request with no `Origin` is not a browser's and is let through.
//!  - **The upgrade is authenticated, by the same layer as every other route.**
//!    It is `Required` in the manifest, so the auth matrix covers it — an
//!    unauthenticated socket is an anonymous, shell-capable agent, and it would
//!    not even show up in the route table it was missing from.
//!  - **A plain GET answers 426 rather than 404.** A route that existed only as
//!    an upgrade handler would be hidden from the generated document and would
//!    answer a bare 404; a client that forgot the upgrade headers then reads it
//!    as "wrong URL" and looks for a path that does not exist.
//!  - **A socket that stops reading is closed, not buffered.** The hub counts
//!    the bytes it has handed over and not yet seen drained, and emits a close
//!    once they pass [`MAX_BUFFERED_BYTES`]; without it, one tab that stopped
//!    draining grows the process by the whole of a turn's output for as long as
//!    it stays open.

use axum::extract::rejection::QueryRejection;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::Response;
use darkwire_core::ErrorKind;
use darkwire_protocol::ws::ErrorCode;
use futures::{SinkExt as _, StreamExt as _};

use crate::errors::HttpError;
use crate::hub::{ConnectOptions, Frame, Outbound};
use crate::queries::WsQuery;
use crate::routes::AppState;
use crate::schema::validated;

/// The origin a browser tab's sessions are recorded under.
const WEB_CHANNEL: &str = "web";

/// What the socket answers a request that did not ask to be upgraded.
const NOT_AN_UPGRADE: &str =
    "This endpoint speaks the DarkWire WebSocket protocol. Connect with an Upgrade request.";

/// Upgrade to the DarkWire WebSocket protocol.
///
/// The query is read and validated *before* the upgrade is accepted, which is
/// the reason it is not left to the hub: a client that sends `?session=` would
/// otherwise get a socket that opens, mints a conversation it did not ask for,
/// and looks to the user like it lost the one they were in.
pub async fn connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    query: Result<Query<WsQuery>, QueryRejection>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, HttpError> {
    if !same_origin(&headers, &uri) {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::Unauthorized,
            ErrorKind::PermissionDenied,
            "This socket only accepts connections from the page this server serves.",
        ));
    }

    let Query(query) = query.map_err(|rejection| {
        // The same 422 every other route answers a failed schema with, rather
        // than axum's bare 400: a client reading one envelope should not have
        // to learn a second one for this route.
        HttpError::unprocessable(rejection.body_text())
    })?;
    let query = validated("query", query)?;

    // Not an upgrade. The authentication layer and the query check have both
    // already run by the time this is reached, so what is left really is only
    // the missing header.
    let upgrade = upgrade.map_err(|_| {
        HttpError::new(
            StatusCode::UPGRADE_REQUIRED,
            ErrorCode::BadRequest,
            ErrorKind::InvalidInput,
            NOT_AN_UPGRADE,
        )
    })?;

    Ok(upgrade.on_upgrade(move |socket| serve(state, socket, query)))
}

/// Whether a request's `Origin`, when it has one, names the host it was sent
/// to.
///
/// Scheme-less, because the listener cannot tell whether TLS was terminated in
/// front of it. `null` never matches: it is what a sandboxed or opaque page
/// sends.
fn same_origin(headers: &HeaderMap, uri: &Uri) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some((scheme, authority)) = origin
        .to_str()
        .ok()
        .and_then(|origin| origin.split_once("://"))
    else {
        return false;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .or_else(|| uri.authority().map(axum::http::uri::Authority::as_str));
    host.is_some_and(|host| {
        without_default_port(scheme, authority) == without_default_port(scheme, host)
    })
}

/// The authority, lowercased, with the port a browser leaves out dropped.
fn without_default_port(scheme: &str, authority: &str) -> String {
    let authority = authority.to_ascii_lowercase();
    let default = match scheme.to_ascii_lowercase().as_str() {
        "http" | "ws" => ":80",
        "https" | "wss" => ":443",
        _ => return authority,
    };
    match authority.strip_suffix(default) {
        Some(bare) => bare.to_owned(),
        None => authority,
    }
}

/// Pumps one socket in both directions until either end stops.
///
/// A single loop rather than two tasks, and that is not only tidier: the hub's
/// close instruction and the client's own frames are then ordered against each
/// other by the same `select!`, so a connection told to go away cannot keep
/// feeding the hub frames on its way out.
async fn serve(state: AppState, socket: WebSocket, query: WsQuery) {
    let (client, mut outbound) = state.hub.connect(ConnectOptions {
        session_key: query.session,
        agent_id: query.agent,
        channel: Some(WEB_CHANNEL.to_owned()),
        max_buffered_bytes: Some(MAX_BUFFERED_BYTES),
        ..ConnectOptions::default()
    });
    let connection_id = client.id().to_owned();
    let (mut sink, mut stream) = socket.split();

    loop {
        tokio::select! {
            instruction = outbound.next() => match instruction {
                Some(Outbound::Text(text)) => {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Some(Outbound::Close(code)) => {
                    // Best effort: the peer this is aimed at is by definition
                    // one that has stopped reading, so a failure to deliver the
                    // courtesy frame changes nothing about the outcome.
                    let _ = sink
                        .send(Message::Close(Some(CloseFrame {
                            code,
                            reason: "client is not reading".into(),
                        })))
                        .await;
                    break;
                }
                // The hub detached this connection.
                None => break,
            },
            frame = stream.next() => match frame {
                // Raw frames: the hub decodes bytes, parses JSON and answers a
                // bad frame with an `error` event on the socket that sent it,
                // so nothing here can fail on input a client controls.
                Some(Ok(Message::Text(text))) => client.receive(Frame::Text(text.to_string())),
                Some(Ok(Message::Binary(bytes))) => {
                    client.receive(Frame::Binary(bytes.to_vec()));
                }
                // Ping and pong are answered by the transport itself.
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(error)) => {
                    tracing::debug!(%error, connection_id, "websocket errored");
                    break;
                }
            },
        }
    }

    // Idempotent, so the close frame above and a peer hang-up can both land
    // here without the hub seeing two departures.
    client.close();
}

/// How much a connection may have queued before it is dropped.
///
/// A client that stops reading is not a client to keep writing to: the frames
/// pile up in this process, and one stalled tab would otherwise be able to
/// exhaust the server's memory. Over the cap the socket closes with 1013 and
/// the connection detaches, which the client sees as a disconnect it can
/// resume from.
pub const MAX_BUFFERED_BYTES: usize = 4 * 1024 * 1024;
