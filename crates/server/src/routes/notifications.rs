//! The notification list a UI shows, and the two ways an entry leaves it.
//!
//! Nothing here creates one. Notifications are raised by things that run
//! without anyone watching — an automation run finishing, an approval expiring
//! — and the route surface is deliberately read-and-dismiss: a `POST
//! /api/notifications` would be an endpoint whose only purpose is letting a
//! client fabricate the server's own reports.

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use darkwire_protocol::rest::{Notification, NotificationListResponse};

use crate::cursor::{
    NotificationCursor, assert_one_paging_mode, decode_notification_cursor,
    encode_notification_cursor, paginate,
};
use crate::errors::HttpError;
use crate::notifications::{ListNotifications, NotificationAfter};
use crate::queries::{IdParams, NotificationListQuery};
use crate::routes::AppState;
use crate::schema::validated;

/// A stored count as the wire carries it.
fn as_u64<T: TryInto<u64>>(value: T) -> u64 {
    value.try_into().unwrap_or(0)
}

/// Reads a query string, reporting a failure in the one error envelope.
fn read_query<T: garde::Validate<Context = ()>>(
    query: Result<Query<T>, QueryRejection>,
) -> Result<T, HttpError> {
    let Query(value) = query.map_err(|error| HttpError::unprocessable(error.body_text()))?;
    validated("query", value)
}

/// Notifications, newest first.
pub async fn list(
    State(state): State<AppState>,
    query: Result<Query<NotificationListQuery>, QueryRejection>,
) -> Result<Json<NotificationListResponse>, HttpError> {
    let query = read_query(query)?;
    assert_one_paging_mode(query.cursor.as_deref(), query.offset)?;

    let unread_only = query.unread == Some(true);
    let after = match query.cursor.as_deref() {
        Some(cursor) => {
            let decoded = decode_notification_cursor(cursor)?;
            Some(NotificationAfter {
                created_at_ms: decoded.created_at_ms,
                id: decoded.id,
            })
        }
        None => None,
    };

    // One more than asked for: the extra row is what decides whether a cursor
    // is issued, and it is dropped rather than returned.
    let rows = state.notifications.list(&ListNotifications {
        limit: Some(i64::from(query.limit) + 1),
        offset: query.offset.map(i64::from),
        after,
        unread_only,
    })?;

    let limit = usize::try_from(query.limit).unwrap_or(usize::MAX);
    let page = paginate(
        rows,
        limit,
        |last| {
            encode_notification_cursor(&NotificationCursor {
                created_at_ms: i64::try_from(last.created_at_ms).unwrap_or(i64::MAX),
                id: last.id.clone(),
            })
        },
        true,
    );

    Ok(Json(NotificationListResponse {
        notifications: page.rows,
        // Always the whole total, never the count of what this page happened to
        // contain: the badge counts what is waiting, not what is on screen.
        unread_count: as_u64(state.notifications.unread_count()?),
        next_cursor: page.next_cursor,
        // A different number from `unread_count` whenever the filter is off,
        // and the pager needs this one: how many rows it is paging through, not
        // how many of them are still unread.
        total: as_u64(state.notifications.count(unread_only)?),
    }))
}

/// Marks one notification read.
pub async fn read(
    State(state): State<AppState>,
    Path(params): Path<IdParams>,
) -> Result<Json<Notification>, HttpError> {
    let updated = state
        .notifications
        .mark_read(&params.id)?
        .ok_or_else(|| HttpError::not_found(format!("No notification \"{}\"", params.id)))?;
    // The updated row rather than a 204, so a client can reconcile one item
    // instead of refetching a list it is in the middle of scrolling.
    Ok(Json(updated))
}

/// Marks every notification read.
pub async fn read_all(State(state): State<AppState>) -> Result<StatusCode, HttpError> {
    state.notifications.mark_all_read()?;
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes one notification.
pub async fn delete(
    State(state): State<AppState>,
    Path(params): Path<IdParams>,
) -> Result<StatusCode, HttpError> {
    if !state.notifications.delete(&params.id)? {
        return Err(HttpError::not_found(format!(
            "No notification \"{}\"",
            params.id
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Empties the list, read and unread alike.
///
/// Its own route rather than a flag on the single delete: `DELETE /x/:id` with
/// an id that means "all of them" is an id a typo can produce. What is *not*
/// here is a confirmation — that belongs to the UI, which is where a person is
/// standing. The server's job is to do exactly what was asked.
pub async fn delete_all(State(state): State<AppState>) -> Result<StatusCode, HttpError> {
    state.notifications.delete_all()?;
    // 204 like its siblings. The count went nowhere useful: a client that just
    // emptied the list refetches it, and a number it cannot act on is a number
    // it would have to invent a use for.
    Ok(StatusCode::NO_CONTENT)
}
