//! The slash commands extensions contribute, listed and run.
//!
//! The first server-side command surface, and it exists because an extension's
//! command is the case the hand-written tables cannot cover. `/new` is written
//! out three times — in the composer, in the terminal and in Telegram — because
//! the surfaces agree on a vocabulary rather than on an implementation. An
//! extension breaks the symmetry, because there is exactly one definition of
//! `/slack-post` and more than one place has to reach it. So this is fetched
//! rather than compiled in.
//!
//! **The composer and the terminal, not Telegram.** Telegram's commands are
//! `bot_command` entities registered with the Bot API, and their names are
//! `[a-z0-9_]` — a namespaced `slack-post` cannot be spelled there. Giving it a
//! second spelling is how one command ends up meaning two things, so it is left
//! out rather than transliterated.
//!
//! Two consequences a reader should expect:
//!
//!  - **The answer is text, not a resource key.** Every built-in command
//!    answers with a key its caller renders; an extension's copy ships with the
//!    extension and the translation layer has never seen it. The same rule a
//!    toolbox's notes follow.
//!  - **A command that reports failure is a 200 with `ok: false`.** An
//!    extension's bug should read as "that did not work" in the composer, not
//!    as a 500 in the console — and the operator typed a command, which is not
//!    the kind of act that deserves an error envelope. A command that does not
//!    *exist* is still a 404, because that is the client asking for something
//!    wrong.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use ghostai_protocol::rest::{CommandListResponse, RunCommandRequest, RunCommandResponse};
use tokio_util::sync::CancellationToken;

use crate::errors::HttpError;
use crate::queries::IdParams;
use crate::routes::AppState;
use crate::schema::parse_body;

/// Every slash command extensions contribute.
///
/// An install with no extensions answers with an empty list rather than
/// refusing: it has no extension commands, which is what the composer asked.
pub async fn list(State(state): State<AppState>) -> Result<Json<CommandListResponse>, HttpError> {
    Ok(Json(CommandListResponse {
        commands: state.runtime.commands(),
    }))
}

/// Run one extension command.
pub async fn run(
    State(state): State<AppState>,
    Path(params): Path<IdParams>,
    body: Bytes,
) -> Result<Json<RunCommandResponse>, HttpError> {
    let raw = serde_json::from_slice(&body)
        .map_err(|error| HttpError::bad_request(format!("Invalid JSON body: {error}")))?;
    let request: RunCommandRequest = parse_body("body", raw)?;

    // Cancelled when this future is dropped, which is what a client navigating
    // away looks like from here: axum drops the handler when the connection
    // goes, and the guard turns that into the one cancellation signal every
    // other long-running route already threads.
    let token = CancellationToken::new();
    let guard = token.clone().drop_guard();

    let pending = state
        .runtime
        .run_command(&params.id, &request, token)
        .ok_or_else(|| HttpError::not_found(format!("No command called \"{}\"", params.id)))?;
    let answer = pending.await?;
    drop(guard);
    Ok(Json(answer))
}
