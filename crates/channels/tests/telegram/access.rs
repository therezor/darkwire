//! The allowlist, which is the security boundary in front of an agent that has
//! the operator's credentials and possibly `exec`.

use ghostai_channels::telegram::access::{AccessList, Requester, parse_allowlist};
use ghostai_core::ErrorKind;

fn entries(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn list(allowlist: &[&str], admins: &[&str]) -> AccessList {
    AccessList::new(&entries(allowlist), &entries(admins)).expect("the list parses")
}

fn private(user_id: i64) -> Requester {
    Requester {
        user_id,
        chat_id: user_id,
    }
}

fn group(user_id: i64, chat_id: i64) -> Requester {
    Requester { user_id, chat_id }
}

// Parsing

#[test]
fn reads_a_bare_id_and_an_id_with_a_label() {
    let parsed = parse_allowlist(&entries(&["4471", "-100123 | the group"])).expect("both parse");

    assert_eq!(parsed[0].id, 4471);
    assert_eq!(parsed[0].label, None);
    assert_eq!(parsed[1].id, -100_123);
    assert_eq!(parsed[1].label.as_deref(), Some("the group"));
}

#[test]
fn keeps_a_label_that_contains_its_own_pipe() {
    let parsed = parse_allowlist(&entries(&["4471|a|b"])).expect("it parses");

    assert_eq!(parsed[0].label.as_deref(), Some("a|b"));
}

#[test]
fn a_malformed_entry_fails_rather_than_being_skipped() {
    // Skipping it would narrow the allowlist by one without saying so, and the
    // symptom — one person the bot has stopped answering — is a long way from
    // the typo that caused it.
    for bad in ["", "me", "4471x", "0", "1.5"] {
        let error = parse_allowlist(&entries(&[bad]))
            .err()
            .unwrap_or_else(|| panic!("{bad:?} is not a Telegram id"));
        assert_eq!(error.kind, ErrorKind::Config);
        assert!(
            error.message.contains("is not a Telegram id"),
            "{}",
            error.message
        );
    }
}

#[test]
fn a_malformed_admin_entry_fails_the_whole_list() {
    let error = AccessList::new(&entries(&["4471"]), &entries(&["nope"]))
        .expect_err("the admin list is parsed too");

    assert_eq!(error.kind, ErrorKind::Config);
}

// permits

#[test]
fn an_empty_allowlist_denies_everyone() {
    let access = list(&[], &[]);

    assert!(access.is_empty());
    assert!(!access.permits(private(4471)));
}

#[test]
fn a_private_chat_needs_only_the_sender() {
    let access = list(&["4471"], &[]);

    assert!(access.permits(private(4471)));
    assert!(!access.permits(private(9999)));
}

#[test]
fn a_group_needs_the_group_and_the_sender() {
    // Being in the room is not permission: checking the chat alone would hand
    // the agent to everyone else in it.
    let access = list(&["4471", "-100123"], &[]);

    assert!(access.permits(group(4471, -100_123)));
    // The group is listed, the sender is not.
    assert!(!access.permits(group(9999, -100_123)));
    // The sender is listed, the group is not.
    assert!(!access.permits(group(4471, -100_999)));
}

#[test]
fn members_are_reported_for_the_startup_line() {
    let access = list(&["4471|me", "-100123"], &[]);

    assert_eq!(access.members().len(), 2);
    assert_eq!(access.members()[0].label.as_deref(), Some("me"));
}

// admits

#[test]
fn an_empty_admin_list_makes_every_allowed_sender_one() {
    // So the distinction costs a single-operator install nothing.
    let access = list(&["4471", "8800"], &[]);

    assert!(access.admits(private(4471)));
    assert!(access.admits(private(8800)));
}

#[test]
fn a_named_admin_list_narrows_it() {
    let access = list(&["4471", "8800"], &["4471"]);

    assert!(access.admits(private(4471)));
    assert!(!access.admits(private(8800)));
    // Still allowed to talk, just not to run the wide commands.
    assert!(access.permits(private(8800)));
}

#[test]
fn an_admin_who_is_not_on_the_allowlist_is_still_refused() {
    let access = list(&["4471"], &["9999"]);

    assert!(!access.admits(private(9999)));
    assert!(!access.permits(private(9999)));
}

// Reporting

#[test]
fn reports_one_line_per_stranger() {
    // One line per id is the whole onboarding path: message the bot, read the
    // log, add the id, restart. A line each time would be a way to fill a disk.
    let access = list(&["4471"], &[]);

    assert!(access.should_report(9999));
    assert!(!access.should_report(9999));
    assert!(access.should_report(8888));
}

#[test]
fn stops_reporting_once_the_table_is_full() {
    // The ids arriving here are chosen by whoever is knocking.
    let access = list(&["4471"], &[]);

    for id in 0..1000 {
        assert!(access.should_report(id), "{id} is the first from that id");
    }
    assert!(!access.should_report(100_000));
}
