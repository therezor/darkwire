//! `SessionStore`: sessions, appending, reading, truncating, forking, turn
//! stats, and the ledger against an older database.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    NOW, assistant_message, call, counter_ids, make_store, make_store_on, text_of, tool_message,
    user_message,
};
use darkwire_core::session_store::{
    AppendOptions, CreateSession, ForkSession, ListSessions, ReadMessages, SessionCursor,
    SessionOrderBy, SessionStore, TruncateResult, TurnStatsRecord, UpdateSession,
    to_stored_message,
};
use darkwire_core::testkit::ManualClock;
use darkwire_core::{Database, ErrorKind};
use darkwire_protocol::messages::{ChatMessage, StopReason, ToolMessage, ToolRole, Usage};
use serde_json::{Map, Value, json};

const NO_OPTIONS: AppendOptions = AppendOptions { turn_id: None };

fn create(title: Option<&str>, origin: Option<&str>) -> CreateSession {
    CreateSession {
        title: title.map(str::to_owned),
        origin: origin.map(str::to_owned),
        ..CreateSession::default()
    }
}

fn in_workspace(workspace_id: &str) -> CreateSession {
    CreateSession {
        workspace_id: Some(workspace_id.to_owned()),
        ..CreateSession::default()
    }
}

fn with_agent(agent_id: &str) -> CreateSession {
    CreateSession {
        agent_id: Some(agent_id.to_owned()),
        ..CreateSession::default()
    }
}

fn keys(store: &SessionStore, options: &ListSessions) -> Vec<String> {
    store
        .list_sessions(options)
        .unwrap()
        .into_iter()
        .map(|summary| summary.session.key)
        .collect()
}

fn sorted_keys(store: &SessionStore, options: &ListSessions) -> Vec<String> {
    let mut keys = keys(store, options);
    keys.sort();
    keys
}

fn seqs(store: &SessionStore, key: &str) -> Vec<i64> {
    store
        .messages(key, &ReadMessages::default())
        .unwrap()
        .iter()
        .map(|record| record.seq)
        .collect()
}

fn append(store: &SessionStore, key: &str, message: ChatMessage) -> i64 {
    store.append(key, message, &NO_OPTIONS).unwrap().seq
}

fn metadata(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

// sessions

#[test]
fn creates_a_session_on_demand_with_defaults() {
    let (store, _) = make_store();
    let session = store
        .ensure_session("web:1", CreateSession::default())
        .unwrap();

    assert_eq!(session.key, "web:1");
    assert_eq!(session.title, "");
    assert_eq!(session.origin, "web");
    assert_eq!(session.workspace_id, "default");
    assert_eq!(session.agent_id, None);
    assert_eq!(session.created_at_ms, NOW);
    assert_eq!(session.updated_at_ms, NOW);
    assert!(session.metadata.is_empty());
}

#[test]
fn is_idempotent_and_does_not_overwrite_an_existing_session() {
    let (store, _) = make_store();
    store
        .ensure_session("web:1", create(Some("First"), Some("telegram")))
        .unwrap();
    let again = store
        .ensure_session("web:1", create(Some("Second"), None))
        .unwrap();

    assert_eq!(again.title, "First");
    assert_eq!(again.origin, "telegram");
}

#[test]
fn rejects_an_empty_session_key() {
    let (store, _) = make_store();
    let error = store
        .ensure_session("", CreateSession::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
}

#[test]
fn returns_none_for_a_session_that_does_not_exist() {
    let (store, _) = make_store();
    assert_eq!(store.get_session("nope").unwrap(), None);
}

#[test]
fn lists_sessions_newest_first_with_message_counts() {
    let (store, _) = make_store();
    store.ensure_session("a", CreateSession::default()).unwrap();
    store.ensure_session("b", CreateSession::default()).unwrap();
    append(&store, "b", user_message("hi"));

    let listed = store.list_sessions(&ListSessions::default()).unwrap();
    assert_eq!(listed.len(), 2);
    let count = |key: &str| {
        listed
            .iter()
            .find(|s| s.session.key == key)
            .unwrap()
            .message_count
    };
    assert_eq!(count("b"), 1);
    assert_eq!(count("a"), 0);
}

#[test]
fn filters_the_listing_by_origin() {
    let (store, _) = make_store();
    store
        .ensure_session("a", create(None, Some("web")))
        .unwrap();
    store
        .ensure_session("t", create(None, Some("telegram")))
        .unwrap();

    let by_origin = ListSessions {
        origin: Some("telegram".to_owned()),
        ..ListSessions::default()
    };
    assert_eq!(keys(&store, &by_origin), ["t"]);
    assert_eq!(sorted_keys(&store, &ListSessions::default()), ["a", "t"]);
}

#[test]
fn lists_every_origin_because_a_hidden_transcript_is_an_undiagnosable_one() {
    // This used to exclude `subagent` and `automation` on the grounds that
    // neither is a conversation a person had. The result was a scheduled run
    // whose turn could not be opened from anywhere. Provenance stays a column.
    let (store, _) = make_store();
    store
        .ensure_session("a", create(None, Some("web")))
        .unwrap();
    store
        .ensure_session("sub", create(None, Some("subagent")))
        .unwrap();
    store
        .ensure_session("auto", create(None, Some("automation")))
        .unwrap();

    assert_eq!(
        sorted_keys(&store, &ListSessions::default()),
        ["a", "auto", "sub"]
    );
    let listed = store.list_sessions(&ListSessions::default()).unwrap();
    let auto = listed.iter().find(|s| s.session.key == "auto").unwrap();
    assert_eq!(auto.session.origin, "automation");
}

#[test]
fn leaves_out_one_origin_when_a_caller_excludes_it_and_counts_the_same_set() {
    let (store, _) = make_store();
    store
        .ensure_session("a", create(None, Some("web")))
        .unwrap();
    store
        .ensure_session("sub", create(None, Some("subagent")))
        .unwrap();
    store
        .ensure_session("auto", create(None, Some("automation")))
        .unwrap();

    let excluding = ListSessions {
        exclude_origin: Some("subagent".to_owned()),
        ..ListSessions::default()
    };
    assert_eq!(sorted_keys(&store, &excluding), ["a", "auto"]);
    // The count runs under the same predicate, or a pager reports a total for
    // a different set than the rows beneath it.
    assert_eq!(store.count_sessions(&excluding).unwrap(), 2);
    // And excluding nothing excludes nothing.
    assert_eq!(store.count_sessions(&ListSessions::default()).unwrap(), 3);
}

#[test]
fn combines_an_exclusion_with_the_other_filters() {
    let (store, _) = make_store();
    store
        .ensure_session(
            "a",
            CreateSession {
                origin: Some("web".to_owned()),
                workspace_id: Some("acme".to_owned()),
                ..CreateSession::default()
            },
        )
        .unwrap();
    store
        .ensure_session(
            "b",
            CreateSession {
                origin: Some("web".to_owned()),
                workspace_id: Some("other".to_owned()),
                ..CreateSession::default()
            },
        )
        .unwrap();
    store
        .ensure_session(
            "sub",
            CreateSession {
                origin: Some("subagent".to_owned()),
                workspace_id: Some("acme".to_owned()),
                ..CreateSession::default()
            },
        )
        .unwrap();

    let options = ListSessions {
        exclude_origin: Some("subagent".to_owned()),
        workspace_id: Some("acme".to_owned()),
        ..ListSessions::default()
    };
    assert_eq!(keys(&store, &options), ["a"]);
}

#[test]
fn still_lists_a_machine_started_origin_when_asked_for_it_by_name() {
    let (store, _) = make_store();
    store
        .ensure_session("a", create(None, Some("web")))
        .unwrap();
    store
        .ensure_session("auto", create(None, Some("automation")))
        .unwrap();

    let options = ListSessions {
        origin: Some("automation".to_owned()),
        ..ListSessions::default()
    };
    assert_eq!(keys(&store, &options), ["auto"]);
    assert_eq!(
        store.get_session("auto").unwrap().unwrap().origin,
        "automation"
    );
}

#[test]
fn paginates_the_listing() {
    let (store, _) = make_store();
    for key in ["a", "b", "c"] {
        store.ensure_session(key, CreateSession::default()).unwrap();
    }

    let page = ListSessions {
        limit: Some(2),
        ..ListSessions::default()
    };
    assert_eq!(store.list_sessions(&page).unwrap().len(), 2);
    let next = ListSessions {
        limit: Some(2),
        offset: Some(2),
        ..ListSessions::default()
    };
    assert_eq!(store.list_sessions(&next).unwrap().len(), 1);
}

#[test]
fn resumes_a_listing_from_a_keyset_cursor() {
    let (store, clock) = make_store();
    for key in ["a", "b", "c"] {
        store.ensure_session(key, CreateSession::default()).unwrap();
        clock.advance(Duration::from_secs(1));
    }

    // Newest first, so the listing runs c, b, a.
    let first = store
        .list_sessions(&ListSessions {
            limit: Some(1),
            ..ListSessions::default()
        })
        .unwrap()
        .swap_remove(0);
    let rest = keys(
        &store,
        &ListSessions {
            after: Some(SessionCursor {
                updated_at_ms: first.session.updated_at_ms,
                key: first.session.key.clone(),
            }),
            ..ListSessions::default()
        },
    );

    assert_eq!(first.session.key, "c");
    assert_eq!(rest, ["b", "a"]);
}

/// The property the cursor exists for, and the one an offset cannot hold: a
/// turn landing between two pages moves a session to the front, which shifts
/// every offset behind it and makes a reader see one row twice.
#[test]
fn does_not_repeat_a_row_when_an_append_reorders_the_listing() {
    let (store, clock) = make_store();
    for key in ["a", "b", "c"] {
        store.ensure_session(key, CreateSession::default()).unwrap();
        clock.advance(Duration::from_secs(1));
    }

    let page = store
        .list_sessions(&ListSessions {
            limit: Some(1),
            ..ListSessions::default()
        })
        .unwrap();
    let cursor = SessionCursor {
        updated_at_ms: page[0].session.updated_at_ms,
        key: page[0].session.key.clone(),
    };

    clock.advance(Duration::from_secs(1));
    append(&store, "a", user_message("a turn landed"));

    let next = keys(
        &store,
        &ListSessions {
            after: Some(cursor),
            ..ListSessions::default()
        },
    );
    // `a` jumped ahead of the cursor and is not served twice; `b`, which the
    // reader had not reached, still arrives.
    assert_eq!(next, ["b"]);
}

#[test]
fn breaks_a_timestamp_tie_by_key_in_one_direction_only() {
    let (store, _) = make_store();
    for key in ["a", "b", "c"] {
        store.ensure_session(key, CreateSession::default()).unwrap();
    }

    // Every row shares `NOW`, so the whole ordering rests on the key column.
    let first = keys(
        &store,
        &ListSessions {
            limit: Some(1),
            ..ListSessions::default()
        },
    );
    assert_eq!(first, ["a"]);

    let rest = keys(
        &store,
        &ListSessions {
            after: Some(SessionCursor {
                updated_at_ms: NOW,
                key: "a".to_owned(),
            }),
            ..ListSessions::default()
        },
    );
    assert_eq!(rest, ["b", "c"]);
}

fn query(text: &str) -> ListSessions {
    ListSessions {
        query: Some(text.to_owned()),
        ..ListSessions::default()
    }
}

#[test]
fn matches_a_title_substring_case_insensitively() {
    let (store, _) = make_store();
    store
        .ensure_session("a", create(Some("Fix the login throttle"), None))
        .unwrap();
    store
        .ensure_session("b", create(Some("Nightly digest"), None))
        .unwrap();
    store
        .ensure_session("c", create(Some("LOGIN rate limits"), None))
        .unwrap();

    assert_eq!(sorted_keys(&store, &query("login")), ["a", "c"]);
}

#[test]
fn treats_a_blank_query_as_no_query_rather_than_as_like_everything() {
    let (store, _) = make_store();
    store
        .ensure_session("a", create(Some("One"), None))
        .unwrap();
    store.ensure_session("b", create(Some(""), None)).unwrap();

    for text in ["", "   "] {
        assert_eq!(sorted_keys(&store, &query(text)), ["a", "b"]);
    }
}

#[test]
fn searches_for_a_wildcard_rather_than_with_one() {
    // Unescaped, `100%` is `LIKE '%100%%'` — which matches every title
    // starting `100`, and `_` would match any single character.
    let (store, _) = make_store();
    store
        .ensure_session("a", create(Some("Down to 100% coverage"), None))
        .unwrap();
    store
        .ensure_session("b", create(Some("100 tests and counting"), None))
        .unwrap();
    store
        .ensure_session("c", create(Some("a_b"), None))
        .unwrap();
    store
        .ensure_session("d", create(Some("axb"), None))
        .unwrap();
    store
        .ensure_session("e", create(Some("back\\slash"), None))
        .unwrap();

    assert_eq!(keys(&store, &query("100%")), ["a"]);
    assert_eq!(keys(&store, &query("a_b")), ["c"]);
    assert_eq!(keys(&store, &query("k\\s")), ["e"]);
}

#[test]
fn orders_by_each_column_it_offers_in_both_directions() {
    let (store, clock) = make_store();
    store
        .ensure_session("a", create(Some("Beta"), None))
        .unwrap();
    clock.advance(Duration::from_millis(1));
    store
        .ensure_session("b", create(Some("alpha"), None))
        .unwrap();
    clock.advance(Duration::from_millis(1));
    store
        .ensure_session("c", create(Some("Gamma"), None))
        .unwrap();

    let order = |order_by: SessionOrderBy, descending: bool| {
        keys(
            &store,
            &ListSessions {
                order_by: Some(order_by),
                descending: Some(descending),
                ..ListSessions::default()
            },
        )
    };

    assert_eq!(keys(&store, &ListSessions::default()), ["c", "b", "a"]);
    assert_eq!(order(SessionOrderBy::Created, false), ["a", "b", "c"]);
    // NOCASE, so `alpha` sorts with the capitals rather than after them.
    assert_eq!(order(SessionOrderBy::Title, false), ["b", "a", "c"]);
    assert_eq!(order(SessionOrderBy::Title, true), ["c", "a", "b"]);
    assert_eq!(order(SessionOrderBy::Updated, true), ["c", "b", "a"]);
}

#[test]
fn refuses_a_cursor_under_an_ordering_it_does_not_address() {
    let (store, _) = make_store();
    store.ensure_session("a", CreateSession::default()).unwrap();
    let cursor = SessionCursor {
        updated_at_ms: NOW,
        key: "a".to_owned(),
    };

    let by_title = store.list_sessions(&ListSessions {
        order_by: Some(SessionOrderBy::Title),
        after: Some(cursor.clone()),
        ..ListSessions::default()
    });
    let error = by_title.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Storage);
    assert_eq!(error.details["orderBy"], "title");
    assert_eq!(error.details["descending"], true);

    let ascending = store.list_sessions(&ListSessions {
        descending: Some(false),
        after: Some(cursor),
        ..ListSessions::default()
    });
    assert_eq!(ascending.unwrap_err().kind, ErrorKind::Storage);
}

#[test]
fn counts_what_the_same_filter_lists_so_a_pager_cannot_disagree_with_its_rows() {
    let (store, _) = make_store();
    let titled = |title: &str, workspace: &str, origin: Option<&str>| CreateSession {
        title: Some(title.to_owned()),
        workspace_id: Some(workspace.to_owned()),
        origin: origin.map(str::to_owned),
        ..CreateSession::default()
    };
    store
        .ensure_session("a", titled("login throttle", "default", None))
        .unwrap();
    store
        .ensure_session("b", titled("login rate limit", "default", None))
        .unwrap();
    store
        .ensure_session("c", titled("nightly digest", "default", None))
        .unwrap();
    store
        .ensure_session("d", titled("login elsewhere", "other", Some("telegram")))
        .unwrap();

    let cases = [
        ListSessions::default(),
        query("login"),
        ListSessions {
            workspace_id: Some("default".to_owned()),
            ..ListSessions::default()
        },
        ListSessions {
            workspace_id: Some("default".to_owned()),
            query: Some("login".to_owned()),
            ..ListSessions::default()
        },
        ListSessions {
            origin: Some("telegram".to_owned()),
            ..ListSessions::default()
        },
        query("nothing matches this"),
    ];
    for options in cases {
        let page = ListSessions {
            limit: Some(100),
            ..options.clone()
        };
        assert_eq!(
            store.count_sessions(&options).unwrap(),
            store.list_sessions(&page).unwrap().len()
        );
    }
    assert_eq!(store.count_sessions(&query("login")).unwrap(), 3);
}

#[test]
fn counts_the_whole_match_rather_than_the_page_in_front_of_it() {
    let (store, _) = make_store();
    for key in ["a", "b", "c", "d", "e"] {
        store.ensure_session(key, CreateSession::default()).unwrap();
    }
    let page = ListSessions {
        limit: Some(2),
        ..ListSessions::default()
    };
    assert_eq!(store.list_sessions(&page).unwrap().len(), 2);
    assert_eq!(store.count_sessions(&page).unwrap(), 5);
}

#[test]
fn patches_only_the_fields_it_is_given() {
    let (store, _) = make_store();
    store
        .ensure_session(
            "a",
            CreateSession {
                title: Some("Title".to_owned()),
                agent_id: Some("p1".to_owned()),
                ..CreateSession::default()
            },
        )
        .unwrap();

    let updated = store
        .update_session(
            "a",
            UpdateSession {
                workspace_id: Some("other".to_owned()),
                ..UpdateSession::default()
            },
        )
        .unwrap();
    assert_eq!(updated.title, "Title");
    assert_eq!(updated.agent_id.as_deref(), Some("p1"));
    assert_eq!(updated.workspace_id, "other");
}

#[test]
fn distinguishes_clearing_an_agent_from_leaving_it_alone() {
    let (store, _) = make_store();
    store.ensure_session("a", with_agent("p1")).unwrap();

    let untouched = store
        .update_session(
            "a",
            UpdateSession {
                title: Some("x".to_owned()),
                ..UpdateSession::default()
            },
        )
        .unwrap();
    assert_eq!(untouched.agent_id.as_deref(), Some("p1"));

    let cleared = store
        .update_session(
            "a",
            UpdateSession {
                agent_id: Some(None),
                ..UpdateSession::default()
            },
        )
        .unwrap();
    assert_eq!(cleared.agent_id, None);
    assert_eq!(store.get_session("a").unwrap().unwrap().agent_id, None);
}

#[test]
fn moves_a_session_to_another_workspace() {
    let (store, _) = make_store();
    store
        .ensure_session(
            "a",
            CreateSession {
                title: Some("Title".to_owned()),
                workspace_id: Some("research".to_owned()),
                ..CreateSession::default()
            },
        )
        .unwrap();

    let updated = store
        .update_session(
            "a",
            UpdateSession {
                workspace_id: Some("archive".to_owned()),
                ..UpdateSession::default()
            },
        )
        .unwrap();
    assert_eq!(updated.workspace_id, "archive");
    assert_eq!(updated.title, "Title");
    assert_eq!(
        store.get_session("a").unwrap().unwrap().workspace_id,
        "archive"
    );
}

#[test]
fn leaves_the_workspace_alone_for_a_patch_that_does_not_name_one() {
    let (store, _) = make_store();
    store.ensure_session("a", in_workspace("research")).unwrap();

    let updated = store
        .update_session(
            "a",
            UpdateSession {
                title: Some("x".to_owned()),
                ..UpdateSession::default()
            },
        )
        .unwrap();
    assert_eq!(updated.workspace_id, "research");
    assert_eq!(
        store.get_session("a").unwrap().unwrap().workspace_id,
        "research"
    );
}

#[test]
fn still_refuses_to_move_a_session_through_ensure_session() {
    // A turn, a frame or a scheduled run arriving with a different workspace
    // must not move a conversation's files out from under it.
    let (store, _) = make_store();
    store.ensure_session("a", in_workspace("research")).unwrap();

    let again = store.ensure_session("a", in_workspace("archive")).unwrap();
    assert_eq!(again.workspace_id, "research");
}

#[test]
fn round_trips_metadata() {
    let (store, _) = make_store();
    store
        .ensure_session(
            "a",
            CreateSession {
                metadata: Some(metadata(&[("topicId", json!(42))])),
                ..CreateSession::default()
            },
        )
        .unwrap();
    assert_eq!(
        store.get_session("a").unwrap().unwrap().metadata,
        metadata(&[("topicId", json!(42))])
    );

    let next = metadata(&[("topicId", json!(43)), ("tags", json!(["x"]))]);
    store
        .update_session(
            "a",
            UpdateSession {
                metadata: Some(next.clone()),
                ..UpdateSession::default()
            },
        )
        .unwrap();
    assert_eq!(store.get_session("a").unwrap().unwrap().metadata, next);
}

#[test]
fn deletes_a_session_and_cascades_to_its_messages() {
    let (store, _) = make_store();
    append(&store, "a", user_message("hi"));

    assert!(store.delete_session("a").unwrap());
    assert_eq!(store.get_session("a").unwrap(), None);
    assert_eq!(store.message_count("a").unwrap(), 0);
    assert!(!store.delete_session("a").unwrap());
}

#[test]
fn deletes_the_subagent_runs_a_session_delegated() {
    let (store, _) = make_store();
    append(&store, "child", user_message("delegated"));
    append(&store, "grandchild", user_message("delegated again"));
    store
        .update_session(
            "child",
            UpdateSession {
                metadata: Some(metadata(&[(
                    "subagentRuns",
                    json!({ "call_2": { "sessionKey": "grandchild", "agentId": "r", "label": "R" } }),
                )])),
                ..UpdateSession::default()
            },
        )
        .unwrap();
    store
        .ensure_session(
            "parent",
            CreateSession {
                metadata: Some(metadata(&[(
                    "subagentRuns",
                    json!({
                        "call_1": { "sessionKey": "child", "agentId": "r", "label": "R" },
                        "gone": { "sessionKey": "already-deleted", "agentId": "r", "label": "R" }
                    }),
                )])),
                ..CreateSession::default()
            },
        )
        .unwrap();

    assert!(store.delete_session("parent").unwrap());
    assert_eq!(store.get_session("child").unwrap(), None);
    assert_eq!(store.get_session("grandchild").unwrap(), None);
    assert_eq!(store.count_sessions(&ListSessions::default()).unwrap(), 0);
}

// appending

#[test]
fn assigns_contiguous_sequence_numbers_from_one() {
    let (store, _) = make_store();
    assert_eq!(append(&store, "s", user_message("one")), 1);
    assert_eq!(append(&store, "s", assistant_message("two", vec![])), 2);
    assert_eq!(append(&store, "s", user_message("three")), 3);
}

#[test]
fn creates_the_session_implicitly() {
    let (store, _) = make_store();
    append(&store, "brand-new", user_message("hi"));
    assert!(store.get_session("brand-new").unwrap().is_some());
}

#[test]
fn appends_a_block_in_one_transaction_with_contiguous_seqs() {
    let (store, _) = make_store();
    let records = store
        .append_many(
            "s",
            vec![
                assistant_message("", vec![call("a"), call("b")]),
                tool_message("a", "read", "x"),
                tool_message("b", "read", "y"),
            ],
            &NO_OPTIONS,
        )
        .unwrap();

    let seqs: Vec<i64> = records.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, [1, 2, 3]);
    assert_eq!(store.message_count("s").unwrap(), 3);
}

#[test]
fn is_a_no_op_for_an_empty_block() {
    let (store, _) = make_store();
    assert!(
        store
            .append_many("s", vec![], &NO_OPTIONS)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.get_session("s").unwrap(), None);
}

#[test]
fn writes_nothing_when_any_message_in_the_block_is_invalid() {
    let (store, _) = make_store();
    append(&store, "s", user_message("first"));

    let bad = ChatMessage::Tool(ToolMessage {
        role: ToolRole,
        tool_call_id: String::new(),
        name: "t".to_owned(),
        content: "x".to_owned(),
        is_error: false,
        truncated: false,
        duration_ms: None,
    });
    let error = store
        .append_many("s", vec![user_message("good"), bad], &NO_OPTIONS)
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert_eq!(error.details["index"], 1);
    assert_eq!(error.details["sessionKey"], "s");

    // The valid sibling must not have landed either.
    assert_eq!(store.message_count("s").unwrap(), 1);
}

#[test]
fn records_the_turn_id_on_every_message_of_a_turn() {
    let (store, _) = make_store();
    let options = AppendOptions {
        turn_id: Some("turn-1".to_owned()),
    };
    let records = store
        .append_many(
            "s",
            vec![
                assistant_message("", vec![call("a")]),
                tool_message("a", "read", "x"),
            ],
            &options,
        )
        .unwrap();
    assert!(
        records
            .iter()
            .all(|r| r.turn_id.as_deref() == Some("turn-1"))
    );
    let stored = store.messages("s", &ReadMessages::default()).unwrap();
    assert!(
        stored
            .iter()
            .all(|r| r.turn_id.as_deref() == Some("turn-1"))
    );
}

#[test]
fn leaves_turn_id_absent_when_none_was_given() {
    let (store, _) = make_store();
    append(&store, "s", user_message("hi"));
    let stored = store.messages("s", &ReadMessages::default()).unwrap();
    assert_eq!(stored[0].turn_id, None);
}

// reading messages

fn seeded() -> SessionStore {
    let (store, _) = make_store();
    append(&store, "s", user_message("one"));
    append(&store, "s", assistant_message("two", vec![]));
    append(&store, "s", user_message("three"));
    store
}

fn read(store: &SessionStore, options: &ReadMessages) -> Vec<i64> {
    store
        .messages("s", options)
        .unwrap()
        .iter()
        .map(|r| r.seq)
        .collect()
}

#[test]
fn returns_messages_in_sequence_order() {
    let store = seeded();
    assert_eq!(read(&store, &ReadMessages::default()), [1, 2, 3]);
}

#[test]
fn pages_forward_from_a_cursor() {
    let store = seeded();
    let options = ReadMessages {
        after_seq: Some(1),
        ..ReadMessages::default()
    };
    assert_eq!(read(&store, &options), [2, 3]);
}

#[test]
fn respects_an_upper_bound() {
    let store = seeded();
    let options = ReadMessages {
        before_seq: Some(3),
        ..ReadMessages::default()
    };
    assert_eq!(read(&store, &options), [1, 2]);
}

#[test]
fn limits_from_the_start_by_default() {
    let store = seeded();
    let options = ReadMessages {
        limit: Some(2),
        ..ReadMessages::default()
    };
    assert_eq!(read(&store, &options), [1, 2]);
}

#[test]
fn takes_the_newest_when_reading_from_the_end_still_in_order() {
    let store = seeded();
    let options = ReadMessages {
        limit: Some(2),
        from_end: true,
        ..ReadMessages::default()
    };
    assert_eq!(read(&store, &options), [2, 3]);
}

#[test]
fn returns_nothing_for_an_unknown_session() {
    let (store, _) = make_store();
    assert!(
        store
            .messages("nope", &ReadMessages::default())
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.message_count("nope").unwrap(), 0);
}

#[test]
fn narrows_a_record_to_the_wire_shape() {
    let (store, _) = make_store();
    let record = store
        .append(
            "s",
            user_message("hi"),
            &AppendOptions {
                turn_id: Some("t1".to_owned()),
            },
        )
        .unwrap();

    let wire = to_stored_message(&record);
    assert_eq!(wire.id, "m1");
    assert_eq!(wire.session_key, "s");
    assert_eq!(wire.seq, 1);
    assert_eq!(wire.created_at_ms, 1_700_000_000_000);
    assert_eq!(wire.turn_id.as_deref(), Some("t1"));
    assert_eq!(wire.message, user_message("hi"));
}

#[test]
fn omits_turn_id_from_the_wire_shape_when_absent() {
    let (store, _) = make_store();
    let wire = to_stored_message(&store.append("s", user_message("hi"), &NO_OPTIONS).unwrap());
    let encoded = serde_json::to_value(&wire).unwrap();
    assert!(encoded.get("turnId").is_none());
}

// durability

#[test]
fn survives_a_reopen_with_tool_call_pairing_intact() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("darkwire.db");
    let clock = Arc::new(ManualClock::at(NOW));

    let first = make_store_on(Database::open(&file).unwrap(), Arc::clone(&clock)).unwrap();
    first
        .append_many(
            "web:1",
            vec![
                user_message("read a.txt and b.txt"),
                assistant_message("", vec![call("a"), call("b")]),
                tool_message("a", "read", "contents of a"),
                tool_message("b", "read", "contents of b"),
                assistant_message("Both read.", vec![]),
            ],
            &AppendOptions {
                turn_id: Some("turn-1".to_owned()),
            },
        )
        .unwrap();
    first
        .update_session(
            "web:1",
            UpdateSession {
                title: Some("Reading files".to_owned()),
                ..UpdateSession::default()
            },
        )
        .unwrap();
    drop(first);

    let second = make_store_on(Database::open(&file).unwrap(), clock).unwrap();
    let history = second.messages("web:1", &ReadMessages::default()).unwrap();

    assert_eq!(
        second.get_session("web:1").unwrap().unwrap().title,
        "Reading files"
    );
    assert_eq!(history.len(), 5);
    assert_eq!(seqs(&second, "web:1"), [1, 2, 3, 4, 5]);
    assert_eq!(
        history[1].message,
        assistant_message("", vec![call("a"), call("b")])
    );
    assert_eq!(
        history[2].message,
        tool_message("a", "read", "contents of a")
    );
}

#[test]
fn continues_the_sequence_after_a_reopen_rather_than_restarting_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("darkwire.db");
    let clock = Arc::new(ManualClock::at(NOW));

    let first = make_store_on(Database::open(&file).unwrap(), Arc::clone(&clock)).unwrap();
    append(&first, "s", user_message("one"));
    drop(first);

    let second = SessionStore::new(
        Database::open(&file).unwrap(),
        clock,
        counter_ids("second-"),
    )
    .unwrap();
    assert_eq!(append(&second, "s", user_message("two")), 2);
}

#[test]
fn creates_the_database_directory_if_it_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("nested").join("darkwire.db");
    let store = make_store_on(
        Database::open(&file).unwrap(),
        Arc::new(ManualClock::at(NOW)),
    )
    .unwrap();
    append(&store, "s", user_message("hi"));
    assert_eq!(store.message_count("s").unwrap(), 1);
}

// clearing and truncating

#[test]
fn removes_messages_but_keeps_the_session_and_its_sequence() {
    let (store, _) = make_store();
    append(&store, "s", user_message("one"));
    append(&store, "s", user_message("two"));

    store.clear_messages("s").unwrap();

    assert_eq!(store.message_count("s").unwrap(), 0);
    // Sequences never rewind: a reconnecting client's stale cursor must not
    // start addressing different messages.
    assert_eq!(append(&store, "s", user_message("three")), 3);
}

#[test]
fn drops_everything_after_the_cut_and_reports_how_much() {
    let (store, _) = make_store();
    for text in ["one", "two", "three", "four"] {
        append(&store, "s", user_message(text));
    }

    assert_eq!(
        store.truncate_after("s", 2).unwrap(),
        TruncateResult { seq: 2, deleted: 2 }
    );
    assert_eq!(seqs(&store, "s"), [1, 2]);
}

#[test]
fn leaves_next_seq_alone_so_sequences_never_rewind() {
    let (store, _) = make_store();
    for text in ["one", "two", "three"] {
        append(&store, "s", user_message(text));
    }

    store.truncate_after("s", 1).unwrap();

    // The gap is deliberate. A reconnecting client holding `after_seq: 2`
    // must not have it start addressing a new message.
    assert_eq!(append(&store, "s", user_message("next")), 4);
}

fn tool_exchange() -> SessionStore {
    let (store, _) = make_store();
    append(&store, "s", user_message("read it"));
    append(&store, "s", assistant_message("", vec![call("a")]));
    append(&store, "s", tool_message("a", "read", "contents"));
    append(&store, "s", assistant_message("done", vec![]));
    store
}

#[test]
fn snaps_back_past_an_assistant_whose_tool_calls_would_be_stranded() {
    let store = tool_exchange();

    // Asking to keep seq 1..2 would leave the assistant declaring `a` with no
    // answer — a provider 400 on the next turn.
    let result = store.truncate_after("s", 2).unwrap();

    assert_eq!(result.seq, 1);
    assert_eq!(seqs(&store, "s"), [1]);
}

#[test]
fn does_not_snap_a_cut_that_is_already_legal() {
    let store = tool_exchange();
    assert_eq!(store.truncate_after("s", 3).unwrap().seq, 3);
}

/// Appends `turns`, each a user message, its tool exchanges and an answer.
/// Each entry of a turn is how many calls one exchange makes.
fn append_turns(store: &SessionStore, turns: &[Vec<usize>]) {
    for (turn, exchanges) in turns.iter().enumerate() {
        append(store, "s", user_message("ask"));
        for (exchange, calls) in exchanges.iter().enumerate() {
            let ids: Vec<String> = (0..*calls)
                .map(|index| format!("t{turn}-e{exchange}-c{index}"))
                .collect();
            append(
                store,
                "s",
                assistant_message("", ids.iter().map(|id| call(id)).collect()),
            );
            for id in &ids {
                append(store, "s", tool_message(id, "read", "x"));
            }
        }
        append(store, "s", assistant_message("done", vec![]));
    }
}

/// The cut the whole prefix gives, read in one go.
fn legal_cut_of_whole_prefix(store: &SessionStore, seq: i64) -> i64 {
    let records = store
        .messages(
            "s",
            &ReadMessages {
                before_seq: Some(seq.saturating_add(1)),
                ..ReadMessages::default()
            },
        )
        .unwrap();
    let messages: Vec<ChatMessage> = records.iter().map(|r| r.message.clone()).collect();
    let end = darkwire_core::history::find_legal_end(&messages);
    if end == records.len() {
        seq
    } else if end == 0 {
        0
    } else {
        records[end - 1].seq
    }
}

#[test]
fn snaps_back_across_a_page_of_tool_results() {
    // One call per result row, and more results than one step of the backward
    // scan reads, so the call that owns them sits a page below the cut.
    let (store, _) = make_store();
    for _ in 0..20 {
        append(&store, "s", user_message("ask"));
        append(&store, "s", assistant_message("fine", vec![]));
    }
    let asked = append(&store, "s", user_message("read them all"));
    let ids: Vec<String> = (0..80).map(|index| format!("c{index}")).collect();
    append(
        &store,
        "s",
        assistant_message("", ids.iter().map(|id| call(id)).collect()),
    );
    let mut last = 0;
    for id in &ids {
        last = append(&store, "s", tool_message(id, "read", "x"));
    }

    // Short of the last result, so the exchange is cut through.
    let result = store.truncate_after("s", last - 1).unwrap();
    assert_eq!(result.seq, asked);
    assert_eq!(seqs(&store, "s").last(), Some(&asked));
}

#[test]
fn keeps_a_cut_whose_exchange_is_whole_across_pages() {
    let (store, _) = make_store();
    append(&store, "s", user_message("read them all"));
    let ids: Vec<String> = (0..80).map(|index| format!("c{index}")).collect();
    append(
        &store,
        "s",
        assistant_message("", ids.iter().map(|id| call(id)).collect()),
    );
    let mut last = 0;
    for id in &ids {
        last = append(&store, "s", tool_message(id, "read", "x"));
    }
    append(&store, "s", assistant_message("done", vec![]));

    assert_eq!(store.truncate_after("s", last).unwrap().seq, last);
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(200))]

    /// The backward scan settles where reading the whole prefix would, for any
    /// history the loop could have written.
    #[test]
    fn the_paged_cut_matches_the_whole_prefix(
        turns in proptest::collection::vec(
            proptest::collection::vec(0usize..4, 0..4),
            0..12,
        ),
        pick in 0usize..200,
    ) {
        let (store, _) = make_store();
        store.ensure_session("s", CreateSession::default()).unwrap();
        append_turns(&store, &turns);
        let count = i64::try_from(store.message_count("s").unwrap()).unwrap();
        let seq = i64::try_from(pick).unwrap() % (count + 2);

        let expected = legal_cut_of_whole_prefix(&store, seq);
        let fork = store.fork_session("s", seq, ForkSession::default()).unwrap();
        proptest::prop_assert_eq!(fork.seq, expected);
        proptest::prop_assert_eq!(store.truncate_after("s", seq).unwrap().seq, expected);
    }
}

#[test]
fn is_a_no_op_past_the_end_and_does_not_bump_the_session() {
    let (store, clock) = make_store();
    append(&store, "s", user_message("one"));
    let before = store.get_session("s").unwrap().unwrap().updated_at_ms;
    clock.advance(Duration::from_secs(5));

    assert_eq!(
        store.truncate_after("s", 99).unwrap(),
        TruncateResult {
            seq: 99,
            deleted: 0
        }
    );
    assert_eq!(store.message_count("s").unwrap(), 1);
    assert_eq!(
        store.get_session("s").unwrap().unwrap().updated_at_ms,
        before
    );
}

#[test]
fn bumps_the_session_when_a_truncation_removed_something() {
    let (store, clock) = make_store();
    append(&store, "s", user_message("one"));
    append(&store, "s", user_message("two"));
    clock.advance(Duration::from_secs(5));

    store.truncate_after("s", 1).unwrap();
    assert_eq!(
        store.get_session("s").unwrap().unwrap().updated_at_ms,
        NOW + 5000
    );
}

#[test]
fn clears_a_session_when_cut_at_zero() {
    let (store, _) = make_store();
    append(&store, "s", user_message("one"));
    append(&store, "s", user_message("two"));

    assert_eq!(store.truncate_after("s", 0).unwrap().deleted, 2);
    assert_eq!(store.message_count("s").unwrap(), 0);
}

#[test]
fn falls_back_to_zero_when_the_first_exchange_is_the_unsplittable_one() {
    let (store, _) = make_store();
    append(&store, "s", assistant_message("", vec![call("a")]));
    append(&store, "s", tool_message("a", "read", "contents"));

    assert_eq!(
        store.truncate_after("s", 1).unwrap(),
        TruncateResult { seq: 0, deleted: 2 }
    );
}

#[test]
fn truncating_rejects_an_unknown_session() {
    let (store, _) = make_store();
    let error = store.truncate_after("nope", 1).unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
    assert_eq!(error.details["sessionKey"], "nope");
}

#[test]
fn leaves_history_readable_with_no_orphaned_tool_result() {
    let (store, _) = make_store();
    append(&store, "s", user_message("read it"));
    append(&store, "s", assistant_message("", vec![call("a")]));
    append(&store, "s", tool_message("a", "read", "contents"));

    store.truncate_after("s", 2).unwrap();

    let history = store.messages("s", &ReadMessages::default()).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].message, user_message("read it"));
}

// forking

#[test]
fn copies_the_prefix_into_a_new_session_and_leaves_the_source_alone() {
    let (store, _) = make_store();
    for text in ["one", "two", "three"] {
        append(&store, "s", user_message(text));
    }

    let fork = store.fork_session("s", 2, ForkSession::default()).unwrap();

    assert_eq!(fork.copied, 2);
    assert_eq!(fork.seq, 2);
    let texts: Vec<String> = store
        .messages(&fork.session.key, &ReadMessages::default())
        .unwrap()
        .iter()
        .map(|r| text_of(&r.message))
        .collect();
    assert_eq!(texts, ["one", "two"]);
    assert_eq!(store.message_count("s").unwrap(), 3);
}

#[test]
fn reseats_sequences_densely_from_one() {
    let (store, _) = make_store();
    for text in ["one", "two", "three"] {
        append(&store, "s", user_message(text));
    }
    store.truncate_after("s", 1).unwrap();
    append(&store, "s", user_message("sparse"));

    let fork = store.fork_session("s", 99, ForkSession::default()).unwrap();

    // The source's seqs are 1 and 4; a fork is a new sequence space.
    assert_eq!(seqs(&store, &fork.session.key), [1, 2]);
    assert_eq!(append(&store, &fork.session.key, user_message("next")), 3);
}

#[test]
fn mints_new_row_ids_but_preserves_turn_ids_and_creation_times() {
    let (store, clock) = make_store();
    let original = store
        .append(
            "s",
            user_message("one"),
            &AppendOptions {
                turn_id: Some("t1".to_owned()),
            },
        )
        .unwrap();
    clock.advance(Duration::from_secs(1));

    let fork = store.fork_session("s", 1, ForkSession::default()).unwrap();
    let copied = store
        .messages(&fork.session.key, &ReadMessages::default())
        .unwrap()
        .swap_remove(0);

    assert_ne!(copied.id, original.id);
    assert_eq!(copied.turn_id.as_deref(), Some("t1"));
    assert_eq!(copied.created_at_ms, original.created_at_ms);
    // The fork is something the user just did.
    assert_eq!(fork.session.updated_at_ms, NOW + 1000);
    assert_eq!(fork.session.created_at_ms, NOW);
}

#[test]
fn inherits_workspace_origin_and_agent() {
    let (store, _) = make_store();
    store
        .ensure_session(
            "s",
            CreateSession {
                origin: Some("cli".to_owned()),
                workspace_id: Some("w2".to_owned()),
                agent_id: Some("p1".to_owned()),
                ..CreateSession::default()
            },
        )
        .unwrap();
    append(&store, "s", user_message("one"));

    let fork = store.fork_session("s", 1, ForkSession::default()).unwrap();

    assert_eq!(fork.session.origin, "cli");
    assert_eq!(fork.session.workspace_id, "w2");
    assert_eq!(fork.session.agent_id.as_deref(), Some("p1"));
    assert_ne!(fork.session.key, "s");
}

#[test]
fn honours_every_override() {
    let (store, _) = make_store();
    append(&store, "s", user_message("one"));

    let fork = store
        .fork_session(
            "s",
            1,
            ForkSession {
                key: Some("chosen".to_owned()),
                title: Some("Branch".to_owned()),
                workspace_id: Some("w9".to_owned()),
                agent_id: Some("p9".to_owned()),
                origin: Some("telegram".to_owned()),
            },
        )
        .unwrap();

    assert_eq!(fork.session.key, "chosen");
    assert_eq!(fork.session.title, "Branch");
    assert_eq!(fork.session.workspace_id, "w9");
    assert_eq!(fork.session.agent_id.as_deref(), Some("p9"));
    assert_eq!(fork.session.origin, "telegram");
}

#[test]
fn records_where_it_came_from() {
    let (store, _) = make_store();
    store
        .ensure_session(
            "s",
            CreateSession {
                metadata: Some(metadata(&[("kept", json!(true))])),
                ..CreateSession::default()
            },
        )
        .unwrap();
    append(&store, "s", user_message("one"));

    let fork = store.fork_session("s", 1, ForkSession::default()).unwrap();

    assert_eq!(
        fork.session.metadata["forkedFrom"],
        json!({ "key": "s", "seq": 1, "atMs": NOW })
    );
    assert_eq!(fork.session.metadata["kept"], json!(true));
}

#[test]
fn carries_the_source_title_and_derives_one_when_the_source_has_none() {
    let (store, _) = make_store();
    append(&store, "s", user_message("why does the login throw"));
    store
        .ensure_session("titled", create(Some("Named already"), None))
        .unwrap();
    append(&store, "titled", user_message("anything"));
    append(&store, "toolish", assistant_message("no user here", vec![]));

    assert_eq!(
        store
            .fork_session("s", 1, ForkSession::default())
            .unwrap()
            .session
            .title,
        "why does the login throw"
    );
    assert_eq!(
        store
            .fork_session("titled", 1, ForkSession::default())
            .unwrap()
            .session
            .title,
        "Named already"
    );
    // No user message to name it after, so the title stays empty for a later
    // message or a rename to claim.
    assert_eq!(
        store
            .fork_session("toolish", 1, ForkSession::default())
            .unwrap()
            .session
            .title,
        ""
    );
}

#[test]
fn forking_snaps_to_a_legal_boundary() {
    let (store, _) = make_store();
    append(&store, "s", user_message("read it"));
    append(&store, "s", assistant_message("", vec![call("a")]));
    append(&store, "s", tool_message("a", "read", "contents"));

    let fork = store.fork_session("s", 2, ForkSession::default()).unwrap();

    assert_eq!(fork.seq, 1);
    assert_eq!(fork.copied, 1);
}

#[test]
fn forks_an_empty_session_at_seq_zero() {
    let (store, _) = make_store();
    append(&store, "s", user_message("one"));

    let fork = store.fork_session("s", 0, ForkSession::default()).unwrap();

    assert_eq!(fork.copied, 0);
    assert_eq!(append(&store, &fork.session.key, user_message("first")), 1);
}

#[test]
fn honours_an_explicit_key_and_refuses_to_overwrite_one() {
    let (store, _) = make_store();
    append(&store, "s", user_message("one"));
    let chosen = || ForkSession {
        key: Some("chosen".to_owned()),
        ..ForkSession::default()
    };

    assert_eq!(
        store.fork_session("s", 1, chosen()).unwrap().session.key,
        "chosen"
    );
    let error = store.fork_session("s", 1, chosen()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert_eq!(error.details["sessionKey"], "chosen");
}

#[test]
fn forking_rejects_an_unknown_source() {
    let (store, _) = make_store();
    let error = store
        .fork_session("nope", 1, ForkSession::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
}

// turn stats

fn stats(turn_id: &str) -> TurnStatsRecord {
    TurnStatsRecord {
        turn_id: turn_id.to_owned(),
        session_key: "s".to_owned(),
        agent_id: "default".to_owned(),
        workspace_id: "default".to_owned(),
        provider: "anthropic".to_owned(),
        model: "claude-opus-5".to_owned(),
        started_at_ms: NOW,
        ended_at_ms: NOW + 1000,
        iterations: 2,
        stop_reason: StopReason::Complete,
        usage: Usage {
            prompt_tokens: 100,
            completion_tokens: 20,
            total_tokens: 120,
            cached_tokens: None,
            reasoning_tokens: None,
        },
        generation_ms: None,
        generation_tokens: None,
        first_token_ms: None,
        error: None,
    }
}

fn store_with_session() -> SessionStore {
    let (store, _) = make_store();
    store.ensure_session("s", CreateSession::default()).unwrap();
    store
}

#[test]
fn records_and_reads_back_a_turn() {
    let store = store_with_session();
    store.record_turn_stats(&stats("t1")).unwrap();
    assert_eq!(store.turn_stats("s", None).unwrap(), [stats("t1")]);
}

#[test]
fn adds_its_columns_to_a_database_an_older_build_created() {
    // `CREATE TABLE IF NOT EXISTS` does nothing to a table that already
    // exists, so a column added to the schema reaches a fresh install and no
    // other.
    //
    // The real schema with every migrated column dropped back off it, rather
    // than a hand-written stub: a stub would drift from the thing it stands in
    // for, and this is exactly the shape an older build left behind. Each new
    // entry in the ledger belongs in this list too.
    //
    // This is also what keeps comments out of the schema's column lists: DROP
    // COLUMN rewrites the stored CREATE TABLE text by offset, and a comment in
    // the list can make the rewrite unparseable.
    let db = Database::in_memory().unwrap();
    let clock = Arc::new(ManualClock::at(NOW));
    drop(make_store_on(db.clone(), Arc::clone(&clock)).unwrap());
    db.execute_batch(
        "ALTER TABLE turn_stats DROP COLUMN workspace_id;
         ALTER TABLE turn_stats DROP COLUMN error;
         ALTER TABLE turn_stats DROP COLUMN generation_ms;
         ALTER TABLE turn_stats DROP COLUMN generation_tokens;
         ALTER TABLE turn_stats DROP COLUMN first_token_ms;",
    )
    .unwrap();
    assert_eq!(db.column_names("turn_stats").unwrap().len(), 14);

    let store = make_store_on(db.clone(), clock).unwrap();
    assert_eq!(db.column_names("turn_stats").unwrap().len(), 19);
    store.ensure_session("s", CreateSession::default()).unwrap();
    // Would fail with `no such column` if the ledger had not run.
    store
        .record_turn_stats(&TurnStatsRecord {
            workspace_id: "research".to_owned(),
            generation_ms: Some(400),
            generation_tokens: Some(88),
            first_token_ms: Some(9000),
            ..stats("t1")
        })
        .unwrap();

    let row = store.turn_stats("s", None).unwrap().swap_remove(0);
    assert_eq!(row.workspace_id, "research");
    assert_eq!(row.generation_ms, Some(400));
    assert_eq!(row.generation_tokens, Some(88));
    assert_eq!(row.first_token_ms, Some(9000));
}

#[test]
fn leaves_the_timings_absent_rather_than_zero_when_nothing_measured_them() {
    let store = store_with_session();
    store.record_turn_stats(&stats("t1")).unwrap();

    let row = store.turn_stats("s", None).unwrap().swap_remove(0);
    assert_eq!(row.generation_ms, None);
    assert_eq!(row.generation_tokens, None);
    assert_eq!(row.first_token_ms, None);
}

#[test]
fn overwrites_the_timings_when_a_turn_ends_twice() {
    let store = store_with_session();
    store
        .record_turn_stats(&TurnStatsRecord {
            generation_ms: Some(400),
            ..stats("t1")
        })
        .unwrap();
    store
        .record_turn_stats(&TurnStatsRecord {
            generation_ms: Some(900),
            ..stats("t1")
        })
        .unwrap();

    assert_eq!(
        store.turn_stats("s", None).unwrap()[0].generation_ms,
        Some(900)
    );
}

#[test]
fn records_the_workspace_the_turn_ran_in_not_where_the_session_ends_up() {
    let (store, _) = make_store();
    store.ensure_session("s", in_workspace("research")).unwrap();
    store
        .record_turn_stats(&TurnStatsRecord {
            workspace_id: "research".to_owned(),
            ..stats("t1")
        })
        .unwrap();

    store
        .update_session(
            "s",
            UpdateSession {
                workspace_id: Some("archive".to_owned()),
                ..UpdateSession::default()
            },
        )
        .unwrap();

    assert_eq!(
        store.turn_stats("s", None).unwrap()[0].workspace_id,
        "research"
    );
    assert_eq!(
        store.get_session("s").unwrap().unwrap().workspace_id,
        "archive"
    );
}

#[test]
fn keeps_the_agent_that_ran_the_turn_not_the_one_the_session_now_names() {
    let (store, _) = make_store();
    store.ensure_session("s", with_agent("reviewer")).unwrap();
    store
        .record_turn_stats(&TurnStatsRecord {
            agent_id: "reviewer".to_owned(),
            ..stats("t1")
        })
        .unwrap();

    store
        .update_session(
            "s",
            UpdateSession {
                agent_id: Some(Some("writer".to_owned())),
                ..UpdateSession::default()
            },
        )
        .unwrap();

    assert_eq!(store.turn_stats("s", None).unwrap()[0].agent_id, "reviewer");
}

#[test]
fn upserts_rather_than_failing_when_a_turn_ends_twice() {
    let store = store_with_session();
    store.record_turn_stats(&stats("t1")).unwrap();
    store
        .record_turn_stats(&TurnStatsRecord {
            iterations: 5,
            ..stats("t1")
        })
        .unwrap();

    let rows = store.turn_stats("s", None).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].iterations, 5);
}

#[test]
fn records_why_a_turn_failed_and_nothing_when_it_did_not() {
    let store = store_with_session();
    store
        .record_turn_stats(&TurnStatsRecord {
            stop_reason: StopReason::Error,
            error: Some("No container runtime is reachable.".to_owned()),
            ..stats("t1")
        })
        .unwrap();
    store.record_turn_stats(&stats("t2")).unwrap();

    let rows = store.turn_stats("s", None).unwrap();
    let by_id = |id: &str| rows.iter().find(|row| row.turn_id == id).unwrap();
    assert_eq!(
        by_id("t1").error.as_deref(),
        Some("No container runtime is reachable.")
    );
    assert_eq!(by_id("t1").stop_reason, StopReason::Error);
    // Not `""` — a turn that succeeded has no reason.
    assert_eq!(by_id("t2").error, None);
}

#[test]
fn returns_the_most_recent_turn_first_and_honours_a_limit() {
    let store = store_with_session();
    for (id, ended) in [("t1", 1), ("t2", 2), ("t3", 3)] {
        store
            .record_turn_stats(&TurnStatsRecord {
                ended_at_ms: NOW + ended,
                ..stats(id)
            })
            .unwrap();
    }

    let ids = |limit: Option<usize>| -> Vec<String> {
        store
            .turn_stats("s", limit)
            .unwrap()
            .into_iter()
            .map(|row| row.turn_id)
            .collect()
    };
    assert_eq!(ids(None), ["t3", "t2", "t1"]);
    assert_eq!(ids(Some(2)), ["t3", "t2"]);
}

#[test]
fn keeps_the_optional_usage_fields_optional() {
    let store = store_with_session();
    store
        .record_turn_stats(&TurnStatsRecord {
            usage: Usage {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
                cached_tokens: None,
                reasoning_tokens: None,
            },
            ..stats("t1")
        })
        .unwrap();
    store
        .record_turn_stats(&TurnStatsRecord {
            usage: Usage {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
                cached_tokens: Some(9),
                reasoning_tokens: Some(4),
            },
            ..stats("t2")
        })
        .unwrap();

    let rows = store.turn_stats("s", None).unwrap();
    let by_id = |id: &str| rows.iter().find(|row| row.turn_id == id).unwrap();
    assert_eq!(by_id("t1").usage.cached_tokens, None);
    assert_eq!(by_id("t2").usage.cached_tokens, Some(9));
    assert_eq!(by_id("t2").usage.reasoning_tokens, Some(4));
}

#[test]
fn sums_usage_per_session_in_one_query() {
    let store = store_with_session();
    store
        .ensure_session("other", CreateSession::default())
        .unwrap();
    store.record_turn_stats(&stats("t1")).unwrap();
    store
        .record_turn_stats(&TurnStatsRecord {
            usage: Usage {
                prompt_tokens: 5,
                completion_tokens: 5,
                total_tokens: 10,
                cached_tokens: None,
                reasoning_tokens: None,
            },
            ..stats("t2")
        })
        .unwrap();
    store
        .record_turn_stats(&TurnStatsRecord {
            session_key: "other".to_owned(),
            ..stats("t3")
        })
        .unwrap();

    let totals = store.session_usage(&["s", "other", "missing"]).unwrap();

    assert_eq!(
        totals["s"],
        Usage {
            prompt_tokens: 105,
            completion_tokens: 25,
            total_tokens: 130,
            cached_tokens: None,
            reasoning_tokens: None,
        }
    );
    assert_eq!(totals["other"].total_tokens, 120);
    // A session with no recorded turns is absent rather than zeroed.
    assert!(!totals.contains_key("missing"));
}

#[test]
fn reports_a_cached_total_only_when_some_turn_reported_one() {
    let store = store_with_session();
    store.record_turn_stats(&stats("t1")).unwrap();
    assert_eq!(
        store.session_usage(&["s"]).unwrap()["s"].cached_tokens,
        None
    );

    store
        .record_turn_stats(&TurnStatsRecord {
            usage: Usage {
                cached_tokens: Some(7),
                ..stats("t2").usage
            },
            ..stats("t2")
        })
        .unwrap();
    assert_eq!(
        store.session_usage(&["s"]).unwrap()["s"].cached_tokens,
        Some(7)
    );
}

#[test]
fn returns_an_empty_map_for_an_empty_page() {
    let (store, _) = make_store();
    assert!(store.session_usage(&[]).unwrap().is_empty());
}

#[test]
fn turn_stats_go_away_with_the_session() {
    let store = store_with_session();
    store.record_turn_stats(&stats("t1")).unwrap();

    store.delete_session("s").unwrap();

    assert!(store.turn_stats("s", None).unwrap().is_empty());
}

#[test]
fn rejects_a_stop_reason_the_wire_does_not_know() {
    let store = store_with_session();
    store.record_turn_stats(&stats("t1")).unwrap();
    store
        .database()
        .execute_batch("UPDATE turn_stats SET stop_reason = 'wandered_off'")
        .unwrap();

    let error = store.turn_stats("s", None).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Storage);
    assert_eq!(error.details["column"], "stop_reason");
}

// corrupt data

#[test]
fn rejects_a_stored_payload_that_no_longer_matches_the_schema() {
    let (store, _) = make_store();
    append(&store, "s", user_message("hi"));

    store
        .database()
        .lock()
        .execute(
            "UPDATE messages SET payload_json = ? WHERE seq = 1",
            ["{\"role\":\"alien\"}"],
        )
        .unwrap();

    let error = store.messages("s", &ReadMessages::default()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Storage);
    assert_eq!(error.details["seq"], 1);
}

#[test]
fn falls_back_to_empty_metadata_rather_than_losing_the_session() {
    let (store, _) = make_store();
    store.ensure_session("s", CreateSession::default()).unwrap();

    for bad in ["not json at all", "[1,2,3]", "null"] {
        store
            .database()
            .lock()
            .execute(
                "UPDATE sessions SET metadata_json = ? WHERE key = ?",
                [bad, "s"],
            )
            .unwrap();
        assert!(store.get_session("s").unwrap().unwrap().metadata.is_empty());
    }
}

#[test]
fn reports_a_column_of_the_wrong_type_as_storage() {
    let (store, _) = make_store();
    store.ensure_session("s", CreateSession::default()).unwrap();
    // STRICT forbids the wrong type in place, so rebuild the row shape around
    // a text `created_at_ms` to model a damaged file.
    store
        .database()
        .execute_batch(
            "CREATE TABLE damaged AS SELECT * FROM sessions;
             UPDATE damaged SET created_at_ms = 'yesterday';
             DROP TABLE sessions;
             ALTER TABLE damaged RENAME TO sessions;",
        )
        .unwrap();

    let error = store.get_session("s").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Storage);
    assert_eq!(error.details["store"], "sessions");
    assert_eq!(error.details["column"], "created_at_ms");
}

// reassigning an agent

fn agent_of(store: &SessionStore, key: &str) -> Option<String> {
    store.get_session(key).unwrap().unwrap().agent_id
}

#[test]
fn moves_every_conversation_bound_to_the_old_id() {
    let (store, _) = make_store();
    store.ensure_session("a", with_agent("reviewer")).unwrap();
    store.ensure_session("b", with_agent("reviewer")).unwrap();

    assert_eq!(store.reassign_agent("reviewer", "code-review").unwrap(), 2);
    assert_eq!(agent_of(&store, "a").as_deref(), Some("code-review"));
    assert_eq!(agent_of(&store, "b").as_deref(), Some("code-review"));
}

#[test]
fn leaves_conversations_bound_to_other_agents_and_unbound_ones_alone() {
    let (store, _) = make_store();
    store
        .ensure_session("mine", with_agent("reviewer"))
        .unwrap();
    store
        .ensure_session("theirs", with_agent("writer"))
        .unwrap();
    store
        .ensure_session("nobody", CreateSession::default())
        .unwrap();

    assert_eq!(store.reassign_agent("reviewer", "code-review").unwrap(), 1);
    assert_eq!(agent_of(&store, "theirs").as_deref(), Some("writer"));
    assert_eq!(agent_of(&store, "nobody"), None);
}

#[test]
fn does_not_rewrite_which_agent_ran_a_past_turn() {
    let (store, _) = make_store();
    store.ensure_session("s", with_agent("reviewer")).unwrap();
    store
        .record_turn_stats(&TurnStatsRecord {
            agent_id: "reviewer".to_owned(),
            ..stats("t1")
        })
        .unwrap();

    store.reassign_agent("reviewer", "code-review").unwrap();

    assert_eq!(agent_of(&store, "s").as_deref(), Some("code-review"));
    assert_eq!(store.turn_stats("s", None).unwrap()[0].agent_id, "reviewer");
}

#[test]
fn reports_nothing_moved_when_no_conversation_names_the_old_id() {
    let (store, _) = make_store();
    store.ensure_session("s", CreateSession::default()).unwrap();
    assert_eq!(store.reassign_agent("reviewer", "code-review").unwrap(), 0);
}

#[test]
fn applies_every_rename_in_one_save() {
    let (store, _) = make_store();
    store.ensure_session("a", with_agent("reviewer")).unwrap();
    store.ensure_session("b", with_agent("writer")).unwrap();

    let moved = store
        .reassign_agents(&[("reviewer", "code-review"), ("writer", "author")])
        .unwrap();

    assert_eq!(moved, 2);
    assert_eq!(agent_of(&store, "a").as_deref(), Some("code-review"));
    assert_eq!(agent_of(&store, "b").as_deref(), Some("author"));
}

#[test]
fn skips_a_rename_that_moves_an_id_onto_itself() {
    let (store, _) = make_store();
    store.ensure_session("a", with_agent("reviewer")).unwrap();

    assert_eq!(
        store.reassign_agents(&[("reviewer", "reviewer")]).unwrap(),
        0
    );
    assert_eq!(agent_of(&store, "a").as_deref(), Some("reviewer"));
}

#[test]
fn does_nothing_at_all_for_an_empty_list() {
    let (store, _) = make_store();
    store.ensure_session("a", with_agent("reviewer")).unwrap();

    assert_eq!(store.reassign_agents(&[]).unwrap(), 0);
    assert_eq!(agent_of(&store, "a").as_deref(), Some("reviewer"));
}

// workspaces, from this side of the table

#[test]
fn counts_and_reassigns_by_workspace_without_bumping_updated_at() {
    let (store, clock) = make_store();
    let created = store.ensure_session("a-1", in_workspace("acme")).unwrap();
    store.ensure_session("a-2", in_workspace("acme")).unwrap();
    clock.advance(Duration::from_secs(1));

    assert_eq!(store.count_by_workspace("acme").unwrap(), 2);
    assert_eq!(store.reassign_workspace("acme", "default").unwrap(), 2);
    assert_eq!(store.count_by_workspace("acme").unwrap(), 0);
    assert_eq!(store.count_by_workspace("default").unwrap(), 2);
    assert_eq!(
        store.get_session("a-1").unwrap().unwrap().updated_at_ms,
        created.updated_at_ms
    );
}

#[test]
fn debug_output_does_not_dump_the_connection() {
    let (store, _) = make_store();
    assert_eq!(format!("{store:?}"), "SessionStore { .. }");
}

// Tasks

mod tasks {
    use super::*;

    use darkwire_protocol::tasks::{TaskItem, TaskStatus};

    fn task(text: &str, status: TaskStatus) -> TaskItem {
        TaskItem {
            text: text.to_owned(),
            status,
        }
    }

    fn plan() -> Vec<TaskItem> {
        vec![
            task("Inspect auth", TaskStatus::Done),
            task("Update sessions", TaskStatus::Doing),
        ]
    }

    #[test]
    fn a_session_with_no_plan_has_no_tasks() {
        let (store, _) = make_store();
        append(&store, "a", user_message("hello"));
        assert_eq!(store.tasks("a").unwrap(), Vec::new());
    }

    /// A conversation nobody has started reads the same as one with no plan, so
    /// nothing that draws this has to tell the two apart.
    #[test]
    fn a_session_that_does_not_exist_has_no_tasks() {
        let (store, _) = make_store();
        assert_eq!(store.tasks("nowhere").unwrap(), Vec::new());
    }

    #[test]
    fn writes_a_list_and_reads_it_back() {
        let (store, _) = make_store();
        append(&store, "a", user_message("hello"));

        store.set_tasks("a", &plan()).unwrap();
        assert_eq!(store.tasks("a").unwrap(), plan());
    }

    #[test]
    fn replaces_rather_than_appends() {
        let (store, _) = make_store();
        append(&store, "a", user_message("hello"));

        store.set_tasks("a", &plan()).unwrap();
        store
            .set_tasks("a", &[task("Ship it", TaskStatus::Todo)])
            .unwrap();

        assert_eq!(
            store.tasks("a").unwrap(),
            vec![task("Ship it", TaskStatus::Todo)]
        );
    }

    #[test]
    fn an_empty_list_clears_it() {
        let (store, _) = make_store();
        append(&store, "a", user_message("hello"));

        store.set_tasks("a", &plan()).unwrap();
        store.set_tasks("a", &[]).unwrap();

        assert_eq!(store.tasks("a").unwrap(), Vec::new());
    }

    /// The reason this does not go through `update_session`: that takes a whole
    /// metadata bag, so the lineage a delegation had just written would be
    /// replaced by whatever the caller read a moment earlier.
    #[test]
    fn leaves_every_other_metadata_key_alone() {
        let (store, _) = make_store();
        append(&store, "a", user_message("hello"));
        store
            .update_session(
                "a",
                UpdateSession {
                    metadata: Some(metadata(&[(
                        "subagentRuns",
                        json!({ "call_1": { "sessionKey": "child", "agentId": "r", "label": "R" } }),
                    )])),
                    ..UpdateSession::default()
                },
            )
            .unwrap();

        store.set_tasks("a", &plan()).unwrap();

        let session = store.get_session("a").unwrap().unwrap();
        assert!(session.metadata.contains_key("subagentRuns"));
        assert_eq!(store.tasks("a").unwrap(), plan());
    }

    /// A list written from a chat command on a conversation the socket minted
    /// but nobody has spoken in must not fail for want of a row.
    #[test]
    fn creates_the_session_row_when_there_is_none() {
        let (store, _) = make_store();
        store.set_tasks("fresh", &plan()).unwrap();
        assert_eq!(store.tasks("fresh").unwrap(), plan());
    }

    /// The plan is a plan for a conversation. Left behind by `/clear`, the next
    /// turn opens with a half-ticked list over an empty transcript.
    #[test]
    fn clearing_the_history_clears_the_plan() {
        let (store, _) = make_store();
        append(&store, "a", user_message("hello"));
        store.set_tasks("a", &plan()).unwrap();

        store.clear_messages("a").unwrap();

        assert_eq!(store.tasks("a").unwrap(), Vec::new());
    }

    /// Lineage is not a plan: a delegated run still happened, and its transcript
    /// is still the thing anyone debugging the answer has to read.
    #[test]
    fn clearing_the_history_leaves_the_lineage_alone() {
        let (store, _) = make_store();
        append(&store, "a", user_message("hello"));
        store
            .update_session(
                "a",
                UpdateSession {
                    metadata: Some(metadata(&[(
                        "subagentRuns",
                        json!({ "call_1": { "sessionKey": "child", "agentId": "r", "label": "R" } }),
                    )])),
                    ..UpdateSession::default()
                },
            )
            .unwrap();

        store.clear_messages("a").unwrap();

        let session = store.get_session("a").unwrap().unwrap();
        assert!(session.metadata.contains_key("subagentRuns"));
    }

    /// The case this exists for: `/regenerate` and `/edit` both truncate, and a
    /// plan written during the turn they are re-running describes answers that
    /// have just been deleted.
    #[test]
    fn regenerating_the_turn_that_wrote_the_plan_drops_it() {
        let (store, _) = make_store();
        append(&store, "a", user_message("do the thing"));
        // The plan is written mid-turn, before the turn appends anything.
        store.set_tasks("a", &plan()).unwrap();
        append(&store, "a", assistant_message("working on it", vec![]));

        // Re-run from the user message: everything after seq 1 goes.
        store.truncate_after("a", 1).unwrap();

        assert_eq!(store.tasks("a").unwrap(), Vec::new());
    }

    /// A plan from an earlier turn still describes work the transcript has a
    /// record of, so a later truncation leaves it alone.
    #[test]
    fn truncating_a_later_turn_leaves_an_earlier_plan_alone() {
        let (store, _) = make_store();
        append(&store, "a", user_message("first"));
        store.set_tasks("a", &plan()).unwrap();
        append(&store, "a", assistant_message("first answer", vec![]));
        let second = append(&store, "a", user_message("second"));
        append(&store, "a", assistant_message("second answer", vec![]));

        store.truncate_after("a", second).unwrap();

        assert_eq!(store.tasks("a").unwrap(), plan());
    }

    /// A truncation that deletes nothing must not delete a plan either.
    #[test]
    fn a_no_op_truncation_leaves_the_plan_alone() {
        let (store, _) = make_store();
        let last = append(&store, "a", user_message("hello"));
        store.set_tasks("a", &plan()).unwrap();

        store.truncate_after("a", last + 100).unwrap();

        assert_eq!(store.tasks("a").unwrap(), plan());
    }

    /// The seq is in the source's sequence space and a fork reseats from 1, so
    /// carrying it over would point the next truncation at the wrong message.
    #[test]
    fn a_fork_does_not_inherit_the_plan() {
        let (store, _) = make_store();
        let first = append(&store, "a", user_message("hello"));
        append(&store, "a", assistant_message("hi", vec![]));
        store.set_tasks("a", &plan()).unwrap();

        let fork = store
            .fork_session("a", first, ForkSession::default())
            .unwrap();

        assert_eq!(store.tasks(&fork.session.key).unwrap(), Vec::new());
        // The source keeps its own.
        assert_eq!(store.tasks("a").unwrap(), plan());
    }

    #[test]
    fn deleting_a_session_takes_its_plan_with_it() {
        let (store, _) = make_store();
        append(&store, "a", user_message("hello"));
        store.set_tasks("a", &plan()).unwrap();

        assert!(store.delete_session("a").unwrap());
        assert_eq!(store.tasks("a").unwrap(), Vec::new());
    }
}
