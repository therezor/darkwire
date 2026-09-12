//! Query shapes as they actually arrive: strings, every one of them.
//!
//! These types exist for one reason — a URL carries no numbers and no booleans,
//! so `limit=50` arrives as `"50"` and reading it as a number is a coercion. The
//! tests below go through the same extractor a request does, rather than
//! through a JSON value: a shape that reads correctly from JSON and not from a
//! query string is a route that answers 422 to every client that sends a page
//! size.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use axum::extract::Query;
use axum::http::Uri;
use garde::Validate as _;
use ghostai_server::queries::{
    DEFAULT_PAGE_LIMIT, DeleteQuery, MAX_PAGE_LIMIT, NotificationListQuery, OptionalPathQuery,
    PageQuery, PathQuery, SessionListQuery, SessionSort, TurnsQuery, WsQuery,
};

/// Reads a shape the way the request path does.
///
/// Through axum's own extractor rather than through a JSON value, because the
/// deserialiser is the thing under test: a shape that reads correctly from JSON
/// and not from a query string is a route that answers 422 to every client that
/// sends a page size.
fn parse<T: serde::de::DeserializeOwned>(query: &str) -> Result<T, String> {
    let uri: Uri = format!("/x?{query}").parse().expect("a well-formed URI");
    Query::try_from_uri(&uri)
        .map(|Query(value)| value)
        .map_err(|rejection| rejection.to_string())
}

// The page

#[test]
fn a_limit_arrives_as_a_string_and_reads_as_a_number() {
    let page: PageQuery = parse("limit=50").expect("a page size a URL can carry");
    assert_eq!(page.limit, 50);
}

#[test]
fn an_absent_limit_takes_the_default() {
    let page: PageQuery = parse("").expect("an empty query is a whole page request");
    assert_eq!(page.limit, DEFAULT_PAGE_LIMIT);
    assert_eq!(page.cursor, None);
    assert_eq!(page.offset, None);
}

#[test]
fn an_offset_is_coerced_the_same_way() {
    let page: PageQuery = parse("offset=20").expect("an offset a URL can carry");
    assert_eq!(page.offset, Some(20));
}

#[test]
fn an_absent_offset_stays_absent_rather_than_becoming_zero() {
    // Zero and "absent" have to stay distinguishable, or a request carrying
    // only a cursor arrives with an offset it never sent and trips the guard
    // that refuses both at once.
    let page: PageQuery = parse("cursor=abc").expect("a cursor-only page request");
    assert_eq!(page.offset, None);
    assert_eq!(page.cursor.as_deref(), Some("abc"));
}

#[test]
fn a_zero_offset_is_kept_as_a_zero() {
    let page: PageQuery = parse("offset=0").expect("an explicit first page");
    assert_eq!(page.offset, Some(0));
}

#[test]
fn a_limit_past_the_cap_is_refused_by_the_rules_rather_than_clamped() {
    let page: PageQuery = parse(&format!("limit={}", MAX_PAGE_LIMIT + 1))
        .expect("it reads as a number before it is judged");
    assert!(page.validate().is_err());
}

#[test]
fn a_limit_of_zero_is_refused() {
    // A page of nothing is a request nobody meant to make.
    let page: PageQuery = parse("limit=0").expect("zero reads as a number");
    assert!(page.validate().is_err());
}

#[test]
fn the_cap_itself_is_accepted() {
    let page: PageQuery = parse(&format!("limit={MAX_PAGE_LIMIT}")).expect("the cap");
    assert!(page.validate().is_ok());
}

#[test]
fn a_limit_that_is_not_a_number_is_refused_while_it_is_read() {
    assert!(parse::<PageQuery>("limit=lots").is_err());
}

// The session listing

#[test]
fn the_session_listing_carries_the_page_fields_directly() {
    // Restated rather than flattened: a query string has no nesting, and
    // flattening buffers every value as a string, which destroys the coercion.
    let query: SessionListQuery = parse("limit=2&offset=4&desc=true").expect("a page of sessions");
    assert_eq!(query.limit, 2);
    assert_eq!(query.offset, Some(4));
    assert_eq!(query.desc, Some(true));
}

#[test]
fn it_carries_an_optional_origin_filter() {
    let query: SessionListQuery = parse("origin=telegram").expect("an origin filter");
    assert_eq!(query.origin.as_deref(), Some("telegram"));
    assert_eq!(query.exclude_origin, None);
}

#[test]
fn it_carries_the_exclusion_the_sidebar_sends() {
    // A shortlist of thirty is a list of conversations, and a delegated run is
    // a step inside one.
    let query: SessionListQuery = parse("excludeOrigin=subagent").expect("an exclusion");
    assert_eq!(query.exclude_origin.as_deref(), Some("subagent"));
}

#[test]
fn an_empty_origin_is_refused_because_it_would_filter_to_nothing() {
    let query: SessionListQuery = parse("origin=").expect("it reads as an empty string");
    assert!(query.validate().is_err());
}

#[test]
fn an_empty_search_is_accepted_because_a_cleared_box_is_not_a_bad_request() {
    let query: SessionListQuery = parse("q=").expect("a cleared search box");
    assert!(query.validate().is_ok());
    assert_eq!(query.q.as_deref(), Some(""));
}

#[test]
fn it_reads_every_column_it_will_order_by() {
    for (raw, expected) in [
        ("sort=updated", SessionSort::Updated),
        ("sort=created", SessionSort::Created),
        ("sort=title", SessionSort::Title),
    ] {
        let query: SessionListQuery = parse(raw).expect(raw);
        assert_eq!(query.sort, Some(expected), "{raw}");
    }
}

#[test]
fn a_column_it_cannot_order_by_is_refused_while_it_is_read() {
    assert!(parse::<SessionListQuery>("sort=size").is_err());
}

#[test]
fn a_direction_that_is_neither_is_refused() {
    assert!(parse::<SessionListQuery>("desc=maybe").is_err());
    let query: SessionListQuery = parse("desc=false").expect("the other direction");
    assert_eq!(query.desc, Some(false));
}

// The notification listing

#[test]
fn the_notification_listing_reads_its_own_flag_and_the_page() {
    let query: NotificationListQuery = parse("unread=true&limit=10").expect("unread only");
    assert_eq!(query.unread, Some(true));
    assert_eq!(query.limit, 10);
}

#[test]
fn a_value_that_is_neither_true_nor_false_is_refused() {
    assert!(parse::<NotificationListQuery>("unread=yes").is_err());
}

#[test]
fn an_absent_flag_means_every_notification() {
    let query: NotificationListQuery = parse("").expect("no filter at all");
    assert_eq!(query.unread, None);
}

// Paths and the socket

#[test]
fn a_path_is_required_where_one_is_the_subject_of_the_request() {
    assert!(parse::<PathQuery>("").is_err());
    let query: PathQuery = parse("path=notes/todo.md").expect("a path");
    assert_eq!(query.path, "notes/todo.md");
    // It is authorised, not authorising: naming a workspace is not a privilege
    // decision here.
    assert_eq!(query.workspace, "default");
}

#[test]
fn an_empty_path_is_refused_by_the_rules() {
    let query: PathQuery = parse("path=").expect("it reads as an empty string");
    assert!(query.validate().is_err());
}

#[test]
fn a_listing_defaults_to_the_workspace_root() {
    let query: OptionalPathQuery = parse("").expect("a listing with no path");
    assert_eq!(query.path, ".");
    assert_eq!(query.workspace, "default");
}

#[test]
fn a_named_workspace_overrides_the_default() {
    let query: PathQuery = parse("path=a.txt&workspace=research").expect("a scoped path");
    assert_eq!(query.workspace, "research");
}

#[test]
fn a_delete_says_the_word_that_only_means_recursion() {
    // A mistyped path, a stale bookmark or a script looping over names cannot
    // recurse without it.
    let bare: DeleteQuery = parse("path=notes").expect("a bare delete");
    assert_eq!(bare.recursive, None);

    let recursive: DeleteQuery = parse("path=notes&recursive=true").expect("a recursive delete");
    assert_eq!(recursive.recursive, Some(true));
}

#[test]
fn the_socket_reads_the_two_parameters_it_takes() {
    let query: WsQuery = parse("session=abc&agent=reviewer").expect("a socket request");
    assert_eq!(query.session.as_deref(), Some("abc"));
    assert_eq!(query.agent.as_deref(), Some("reviewer"));
}

#[test]
fn an_empty_session_is_refused_rather_than_silently_minting_one() {
    // A client that sends `?session=` should get an error it can read instead
    // of a socket that opens, mints a session it did not ask for, and looks
    // like it lost the conversation.
    let query: WsQuery = parse("session=").expect("it reads as an empty string");
    assert!(query.validate().is_err());
}

#[test]
fn a_socket_request_that_names_nothing_is_legal() {
    let query: WsQuery = parse("").expect("a connection that asks the hub to mint a session");
    assert!(query.validate().is_ok());
    assert_eq!(query.session, None);
}

// Turns

#[test]
fn the_turn_listing_takes_a_bound_and_no_cursor() {
    let query: TurnsQuery = parse("limit=10").expect("a bounded turn listing");
    assert_eq!(query.limit, 10);
    // Accepting a `cursor` that is then ignored would put a parameter in the
    // document the server does not honour.
    assert!(parse::<TurnsQuery>("cursor=abc").is_ok());
    let defaulted: TurnsQuery = parse("").expect("an unbounded request");
    assert_eq!(defaulted.limit, DEFAULT_PAGE_LIMIT);
}

#[test]
fn the_turn_listing_obeys_the_same_cap() {
    let query: TurnsQuery =
        parse(&format!("limit={}", MAX_PAGE_LIMIT + 1)).expect("it reads as a number");
    assert!(query.validate().is_err());
}
