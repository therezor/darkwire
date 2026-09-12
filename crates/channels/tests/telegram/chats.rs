//! What the channel remembers about one chat, and how a conversation moves.

use ghostai_channels::telegram::chats::{
    ChatBook, RenderPrefs, default_session_key, new_session_key, owns_session_key,
};

#[test]
fn a_chats_default_conversation_is_stable_so_it_survives_a_restart() {
    assert_eq!(default_session_key("telegram", 4471), "telegram:4471");
    assert_eq!(
        default_session_key("telegram", -100_123),
        "telegram:-100123"
    );
}

#[test]
fn a_fresh_conversation_keeps_the_chat_it_came_from_in_its_name() {
    // A suffix rather than a wholly new key, so a glance at the session list
    // still says which chat a conversation came from.
    let key = new_session_key("telegram", 4471, "abc");

    assert_eq!(key, "telegram:4471:abc");
    assert!(key.starts_with(&default_session_key("telegram", 4471)));
}

#[test]
fn the_namespacing_the_manager_applies_is_idempotent_on_both_forms() {
    // One form travels everywhere — publish, control, and the store the
    // commands read. Two forms in flight would be a bug factory.
    for key in [
        default_session_key("telegram", 4471),
        new_session_key("telegram", 4471, "abc"),
    ] {
        assert!(owns_session_key("telegram", &key));
    }
}

#[test]
fn a_key_from_another_channel_is_not_ours() {
    // The manager would happily namespace `web-abc` into `telegram:web-abc` — a
    // real conversation, empty, that nothing explains.
    assert!(!owns_session_key("telegram", "web-abc"));
    assert!(!owns_session_key("telegram", "web:1"));
    assert!(!owns_session_key("telegram", "telegramX:1"));
}

#[test]
fn a_chat_is_created_on_first_sight_attached_to_its_default() {
    let mut book = ChatBook::new("telegram");

    let state = book.snapshot(4471);

    assert_eq!(state.session_key, "telegram:4471");
    assert_eq!(state.live_message_id, None);
    assert_eq!(state.live_turn_id, None);
    assert_eq!(state.last_edit_ms, 0);
    assert_eq!(
        state.prefs,
        RenderPrefs {
            progress: true,
            markdown: true
        }
    );
}

#[test]
fn a_chat_seen_twice_is_the_same_chat() {
    let mut book = ChatBook::new("telegram");

    book.for_chat(4471).last_edit_ms = 99;

    assert_eq!(book.snapshot(4471).last_edit_ms, 99);
    // A different chat is a different row.
    assert_eq!(book.snapshot(8800).last_edit_ms, 0);
}

#[test]
fn attaching_points_the_chat_at_another_conversation() {
    let mut book = ChatBook::new("telegram");

    book.attach(4471, "telegram:4471:abc");

    assert_eq!(book.snapshot(4471).session_key, "telegram:4471:abc");
}

#[test]
fn attaching_releases_the_message_the_old_turn_was_filling_in() {
    // Editing it after a switch would rewrite an answer the reader is still
    // scrolled to.
    let mut book = ChatBook::new("telegram");
    {
        let state = book.for_chat(4471);
        state.live_message_id = Some(77);
        state.live_turn_id = Some("turn-1".to_owned());
    }

    book.attach(4471, "telegram:4471:abc");

    let state = book.snapshot(4471);
    assert_eq!(state.live_message_id, None);
    assert_eq!(state.live_turn_id, None);
}

#[test]
fn attaching_a_chat_it_has_not_seen_creates_it() {
    let mut book = ChatBook::new("telegram");

    book.attach(9000, "telegram:9000:abc");

    assert_eq!(book.snapshot(9000).session_key, "telegram:9000:abc");
}

#[test]
fn preferences_default_to_the_pair_a_chat_app_can_render() {
    assert_eq!(
        RenderPrefs::default(),
        RenderPrefs {
            progress: true,
            markdown: true
        }
    );
}
