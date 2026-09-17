//! Buttons, and the token table their 64-byte `callback_data` points into.

use std::sync::Arc;
use std::time::Duration;

use darkwire_channels::telegram::menus::{
    CallbackLookup, CallbackPayload, CallbackRefusal, CallbackStore, DEFAULT_PAGE_SIZE,
    MAX_CALLBACK_ENTRIES, MenuKind, PickerRow, approval_keyboard, confirm_keyboard, picker,
    picker_keyboard,
};
use darkwire_core::clock::Clock;
use darkwire_core::testkit::ManualClock;
use darkwire_protocol::ApprovalScope;

const NOW: i64 = 1_700_000_000_000;

fn store() -> (CallbackStore, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::at(NOW));
    (
        CallbackStore::new(Arc::clone(&clock) as Arc<dyn Clock>),
        clock,
    )
}

fn session(key: &str) -> CallbackPayload {
    CallbackPayload::Session {
        session_key: key.to_owned(),
    }
}

fn rows(count: usize) -> Vec<PickerRow> {
    (0..count)
        .map(|index| PickerRow {
            label: format!("row {index}"),
            current: index == 0,
            payload: session(&format!("telegram:{index}")),
        })
        .collect()
}

// The token table

#[test]
fn a_token_is_short_whatever_it_points_at() {
    // Telegram caps `callback_data` at 64 bytes, and a model-authored call id
    // with a session key beside it does not fit.
    let (store, _clock) = store();
    let long = "telegram:-1001234567890:0193f0c7-4b2a-7e11-9a3d-6f0b1c2d3e4f";

    let token = store.put(
        4471,
        CallbackPayload::Approve {
            call_id: "toolu_01A9bC2dE3fG4hI5jK6lM7nO8pQ9rS0tU".to_owned(),
            session_key: long.to_owned(),
            approved: true,
            scope: ApprovalScope::Once,
        },
        None,
    );

    assert!(token.len() <= 6, "{token:?}");
    assert!(token.len() < 64);
}

#[test]
fn a_token_resolves_to_what_it_was_filed_with() {
    let (store, _clock) = store();

    let token = store.put(4471, session("telegram:4471:abc"), None);

    assert_eq!(
        store.take(&token, 4471),
        CallbackLookup::Found(session("telegram:4471:abc"))
    );
}

#[test]
fn an_unknown_token_reads_as_expired() {
    // A reader pressing a button from yesterday should be told the menu is
    // gone, not that it never existed.
    let (store, _clock) = store();

    assert_eq!(
        store.take("nope", 4471),
        CallbackLookup::Refused(CallbackRefusal::Expired)
    );
}

#[test]
fn a_stale_button_says_so_rather_than_acting() {
    let (store, clock) = store();
    let token = store.put(4471, session("telegram:4471:abc"), None);

    clock.advance(Duration::from_mins(31));

    assert_eq!(
        store.take(&token, 4471),
        CallbackLookup::Refused(CallbackRefusal::Expired)
    );
    // And it is dropped, so a second press does not walk the table again.
    assert!(store.is_empty());
}

#[test]
fn a_button_belongs_to_the_chat_it_was_posted_in() {
    // Anybody in a group can tap a button the bot posted.
    let (store, _clock) = store();
    let token = store.put(4471, session("telegram:4471:abc"), None);

    assert_eq!(
        store.take(&token, 9999),
        CallbackLookup::Refused(CallbackRefusal::WrongChat)
    );
    // Refused, not consumed: the chat it belongs to can still press it.
    assert_eq!(
        store.take(&token, 4471),
        CallbackLookup::Found(session("telegram:4471:abc"))
    );
}

#[test]
fn an_approval_answers_once() {
    let (store, _clock) = store();
    let token = store.put(
        4471,
        CallbackPayload::Approve {
            call_id: "call-1".to_owned(),
            session_key: "telegram:4471".to_owned(),
            approved: true,
            scope: ApprovalScope::Once,
        },
        None,
    );

    assert!(matches!(store.take(&token, 4471), CallbackLookup::Found(_)));
    assert_eq!(
        store.take(&token, 4471),
        CallbackLookup::Refused(CallbackRefusal::Expired)
    );
}

#[test]
fn a_paging_button_stays_live_so_the_reader_can_go_back() {
    let (store, _clock) = store();
    let token = store.put(
        4471,
        CallbackPayload::Page {
            menu: MenuKind::Sessions,
            offset: 8,
        },
        None,
    );

    assert!(matches!(store.take(&token, 4471), CallbackLookup::Found(_)));
    assert!(matches!(store.take(&token, 4471), CallbackLookup::Found(_)));
}

#[test]
fn an_approval_button_inherits_the_gates_own_deadline() {
    // Rather than outliving the request it answers.
    let (store, clock) = store();
    let token = store.put(
        4471,
        CallbackPayload::Approve {
            call_id: "call-1".to_owned(),
            session_key: "telegram:4471".to_owned(),
            approved: true,
            scope: ApprovalScope::Once,
        },
        Some(NOW + 1000),
    );

    clock.advance(Duration::from_secs(2));

    assert_eq!(
        store.take(&token, 4471),
        CallbackLookup::Refused(CallbackRefusal::Expired)
    );
}

#[test]
fn forgetting_a_chat_drops_only_that_chats_buttons() {
    let (store, _clock) = store();
    let mine = store.put(4471, session("a"), None);
    let theirs = store.put(8800, session("b"), None);

    store.forget(4471);

    assert_eq!(
        store.take(&mine, 4471),
        CallbackLookup::Refused(CallbackRefusal::Expired)
    );
    assert!(matches!(
        store.take(&theirs, 8800),
        CallbackLookup::Found(_)
    ));
}

#[test]
fn the_table_is_bounded_oldest_first() {
    // Menus are cheap to produce and a chat could open a hundred.
    let (store, _clock) = store();
    let first = store.put(4471, session("first"), None);
    for index in 0..MAX_CALLBACK_ENTRIES + 10 {
        store.put(4471, session(&format!("row {index}")), None);
    }

    assert!(store.len() <= MAX_CALLBACK_ENTRIES);
    assert_eq!(
        store.take(&first, 4471),
        CallbackLookup::Refused(CallbackRefusal::Expired)
    );
}

#[test]
fn eviction_takes_the_expired_before_the_merely_old() {
    let (store, clock) = store();
    // A short-lived batch, then a long-lived one.
    for index in 0..MAX_CALLBACK_ENTRIES {
        store.put(4471, session(&format!("old {index}")), Some(NOW + 500));
    }
    let keeper = store.put(4471, session("keeper"), Some(NOW + 10_000_000));
    clock.advance(Duration::from_secs(1));
    for index in 0..10 {
        store.put(
            4471,
            session(&format!("new {index}")),
            Some(NOW + 10_000_000),
        );
    }

    assert!(store.len() <= MAX_CALLBACK_ENTRIES);
    assert!(matches!(
        store.take(&keeper, 4471),
        CallbackLookup::Found(_)
    ));
}

// Keyboards

#[test]
fn a_picker_shows_one_page_and_marks_where_you_are() {
    let (store, _clock) = store();

    let keyboard = picker(&rows(3), MenuKind::Sessions, 4471, &store);

    assert_eq!(keyboard.inline_keyboard.len(), 3);
    assert_eq!(keyboard.inline_keyboard[0][0].text, "• row 0");
    assert_eq!(keyboard.inline_keyboard[1][0].text, "row 1");
    // No arrows: everything fits.
    assert!(keyboard.inline_keyboard.iter().all(|row| row.len() == 1));
}

#[test]
fn a_picker_adds_only_the_arrows_it_needs() {
    let (store, _clock) = store();
    let rows = rows(20);

    let first = picker_keyboard(
        &rows,
        MenuKind::Sessions,
        4471,
        &store,
        0,
        DEFAULT_PAGE_SIZE,
    );
    let arrows = first.inline_keyboard.last().expect("a row");
    assert_eq!(arrows.len(), 1);
    assert_eq!(arrows[0].text, "Next »");

    let middle = picker_keyboard(
        &rows,
        MenuKind::Sessions,
        4471,
        &store,
        8,
        DEFAULT_PAGE_SIZE,
    );
    let arrows = middle.inline_keyboard.last().expect("a row");
    assert_eq!(
        arrows
            .iter()
            .map(|button| button.text.as_str())
            .collect::<Vec<_>>(),
        vec!["« Prev", "Next »"]
    );

    let last = picker_keyboard(
        &rows,
        MenuKind::Sessions,
        4471,
        &store,
        16,
        DEFAULT_PAGE_SIZE,
    );
    let arrows = last.inline_keyboard.last().expect("a row");
    assert_eq!(arrows.len(), 1);
    assert_eq!(arrows[0].text, "« Prev");
}

#[test]
fn a_paging_arrow_is_itself_a_token() {
    // So paging costs a round trip and no state on the message.
    let (store, _clock) = store();
    let keyboard = picker_keyboard(
        &rows(20),
        MenuKind::Agents,
        4471,
        &store,
        0,
        DEFAULT_PAGE_SIZE,
    );
    let next = &keyboard.inline_keyboard.last().expect("arrows")[0];

    assert_eq!(
        store.take(&next.callback_data, 4471),
        CallbackLookup::Found(CallbackPayload::Page {
            menu: MenuKind::Agents,
            offset: 8
        })
    );
}

#[test]
fn an_offset_past_the_end_shows_an_empty_page_rather_than_failing() {
    let (store, _clock) = store();

    let keyboard = picker_keyboard(&rows(3), MenuKind::Sessions, 4471, &store, 99, 8);

    // Only the way back.
    assert_eq!(keyboard.inline_keyboard.len(), 1);
    assert_eq!(keyboard.inline_keyboard[0][0].text, "« Prev");
}

#[test]
fn an_empty_listing_produces_an_empty_keyboard() {
    let (store, _clock) = store();

    let keyboard = picker(&[], MenuKind::Models, 4471, &store);

    assert!(keyboard.inline_keyboard.is_empty());
}

#[test]
fn an_approval_offers_two_scopes_and_one_refusal() {
    // Denial is `once` on purpose: a "deny for the session" one tap from "deny
    // once", on a phone, is a way to silently disable a tool and not find out
    // for an hour.
    let (store, _clock) = store();

    let keyboard = approval_keyboard("call-1", "telegram:4471", 4471, &store, NOW + 60_000);

    let labels: Vec<&str> = keyboard
        .inline_keyboard
        .iter()
        .flatten()
        .map(|button| button.text.as_str())
        .collect();
    assert_eq!(labels, vec!["✅ Once", "✅ This session", "⛔ Deny"]);

    let deny = &keyboard.inline_keyboard[1][0];
    let CallbackLookup::Found(CallbackPayload::Approve {
        approved, scope, ..
    }) = store.take(&deny.callback_data, 4471)
    else {
        panic!("the refusal is an approval payload");
    };
    assert!(!approved);
    assert_eq!(scope, ApprovalScope::Once);
}

#[test]
fn a_confirmation_is_one_button_that_says_what_it_does() {
    let (store, _clock) = store();

    let keyboard = confirm_keyboard(
        4471,
        &store,
        CallbackPayload::Delete {
            session_key: "telegram:4471:abc".to_owned(),
        },
        None,
    );

    assert_eq!(keyboard.inline_keyboard.len(), 1);
    assert_eq!(keyboard.inline_keyboard[0][0].text, "Yes, delete it");
}

#[test]
fn a_confirmation_takes_its_own_wording() {
    let (store, _clock) = store();

    let keyboard = confirm_keyboard(
        4471,
        &store,
        CallbackPayload::Delete {
            session_key: "a".to_owned(),
        },
        Some("Yes, remove it"),
    );

    assert_eq!(keyboard.inline_keyboard[0][0].text, "Yes, remove it");
}
