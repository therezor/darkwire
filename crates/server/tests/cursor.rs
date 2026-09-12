//! Opaque pagination cursors: they round-trip, they do not read as the value
//! they carry, and anything this server did not issue is a 400.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ghostai_server::cursor::{
    AutomationRunCursor, MessageCursor, NotificationCursor, SessionListCursor,
    assert_one_paging_mode, decode_automation_run_cursor, decode_message_cursor,
    decode_notification_cursor, decode_session_cursor, encode_automation_run_cursor,
    encode_message_cursor, encode_notification_cursor, encode_session_cursor, paginate,
};

fn raw(json: &str) -> String {
    URL_SAFE_NO_PAD.encode(json)
}

#[test]
fn round_trips_a_message_position() {
    let cursor = MessageCursor { seq: 42 };
    assert_eq!(
        decode_message_cursor(&encode_message_cursor(&cursor)).unwrap(),
        cursor
    );
}

#[test]
fn round_trips_a_session_position() {
    let cursor = SessionListCursor {
        updated_at_ms: 1_700_000_000_000,
        key: "web-1".to_owned(),
    };
    assert_eq!(
        decode_session_cursor(&encode_session_cursor(&cursor)).unwrap(),
        cursor
    );
}

#[test]
fn round_trips_a_notification_position() {
    let cursor = NotificationCursor {
        created_at_ms: 1_700_000_000_000,
        id: "n1".to_owned(),
    };
    assert_eq!(
        decode_notification_cursor(&encode_notification_cursor(&cursor)).unwrap(),
        cursor
    );
}

#[test]
fn round_trips_an_automation_run_position() {
    let cursor = AutomationRunCursor {
        started_at_ms: 1_700_000_000_000,
        id: "run-1".to_owned(),
    };
    assert_eq!(
        decode_automation_run_cursor(&encode_automation_run_cursor(&cursor)).unwrap(),
        cursor
    );
}

#[test]
fn does_not_look_like_the_value_it_carries() {
    // Opaque on purpose: a cursor that reads as `42` is a cursor a client does
    // arithmetic on, and then the server can never change what one addresses.
    assert!(!encode_message_cursor(&MessageCursor { seq: 42 }).contains("42"));
}

#[test]
fn refuses_anything_this_server_did_not_issue() {
    // Never a silent restart from the top: that pages a client through the same
    // first page forever, which reads as a hung UI rather than a bad request.
    for (name, cursor) in [
        ("not base64 at all", "@@@@".to_owned()),
        ("base64 that is not JSON", raw("hello")),
        ("JSON that is not an object", raw("[1,2]")),
        ("an object with the wrong field", raw("{\"x\":1}")),
        ("a seq that is not an integer", raw("{\"s\":1.5}")),
    ] {
        let error = decode_message_cursor(&cursor)
            .err()
            .unwrap_or_else(|| panic!("{name} should not decode"));
        assert_eq!(error.status, 400, "{name}");
        assert!(error.message.to_lowercase().contains("cursor"), "{name}");
    }
}

#[test]
fn refuses_a_session_cursor_missing_its_key() {
    assert!(decode_session_cursor(&raw("{\"u\":1}")).is_err());
    // Present but empty is the same thing: it addresses no row.
    assert!(decode_session_cursor(&raw("{\"u\":1,\"k\":\"\"}")).is_err());
}

#[test]
fn refuses_a_notification_cursor_missing_its_id() {
    assert!(decode_notification_cursor(&raw("{\"c\":1}")).is_err());
    assert!(decode_notification_cursor(&raw("{\"c\":1,\"i\":\"\"}")).is_err());
}

#[test]
fn refuses_an_automation_run_cursor_missing_its_id() {
    assert!(decode_automation_run_cursor(&raw("{\"s\":1}")).is_err());
    assert!(decode_automation_run_cursor(&raw("{\"s\":1,\"i\":\"\"}")).is_err());
}

#[test]
fn refuses_a_cursor_for_the_wrong_listing() {
    // A message cursor handed to the session listing decodes as base64 and as
    // JSON and still does not address a position. It has to be rejected on its
    // fields, not on its encoding.
    let message = encode_message_cursor(&MessageCursor { seq: 1 });
    assert!(decode_session_cursor(&message).is_err());
}

#[test]
fn accepts_a_cursor_a_client_re_padded_on_its_way_back() {
    let cursor = encode_message_cursor(&MessageCursor { seq: 7 });
    let padded = format!("{cursor}{}", "=".repeat((4 - cursor.len() % 4) % 4));
    assert_eq!(decode_message_cursor(&padded).unwrap().seq, 7);
}

#[test]
fn refuses_a_request_naming_both_paging_modes() {
    // A cursor addresses a position and an offset counts rows from the top, so
    // a request carrying both asks for a page relative to a page.
    assert!(assert_one_paging_mode(Some("abc"), Some(20)).is_err());
    assert!(assert_one_paging_mode(Some("abc"), None).is_ok());
    assert!(assert_one_paging_mode(None, Some(20)).is_ok());
    assert!(assert_one_paging_mode(None, None).is_ok());
}

#[test]
fn paginate_drops_the_over_fetched_row_and_issues_a_cursor_for_the_last_kept() {
    let page = paginate(vec![1, 2, 3, 4], 3, |last| format!("after-{last}"), true);
    assert_eq!(page.rows, [1, 2, 3]);
    assert_eq!(page.next_cursor.as_deref(), Some("after-3"));
}

#[test]
fn paginate_issues_no_cursor_when_the_page_is_the_last_one() {
    let page = paginate(vec![1, 2], 3, |last| format!("after-{last}"), true);
    assert_eq!(page.rows, [1, 2]);
    assert_eq!(page.next_cursor, None);
}

#[test]
fn paginate_issues_no_cursor_for_an_ordering_a_cursor_cannot_address() {
    // Sessions sorted by title page by offset; a cursor handed back there would
    // be one that cannot be followed.
    let page = paginate(vec![1, 2, 3, 4], 3, |last| format!("after-{last}"), false);
    assert_eq!(page.rows, [1, 2, 3]);
    assert_eq!(page.next_cursor, None);
}

#[test]
fn paginate_issues_no_cursor_for_an_empty_page() {
    let page = paginate(Vec::<u8>::new(), 3, |last| format!("after-{last}"), true);
    assert!(page.rows.is_empty());
    assert_eq!(page.next_cursor, None);
}
