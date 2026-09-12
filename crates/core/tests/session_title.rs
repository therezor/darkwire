//! Deriving a session title from the first message.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_core::session_title::{
    MAX_TITLE_CHARS, derive_session_title, derive_session_title_within,
};

fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

#[test]
fn returns_a_short_message_unchanged() {
    assert_eq!(
        derive_session_title("fix the login bug"),
        "fix the login bug"
    );
}

#[test]
fn returns_nothing_for_an_empty_message() {
    assert_eq!(derive_session_title(""), "");
    assert_eq!(derive_session_title("   \n\t  "), "");
}

#[test]
fn collapses_newlines_and_runs_of_whitespace() {
    assert_eq!(
        derive_session_title("fix   the\n\nlogin\tbug"),
        "fix the login bug"
    );
}

#[test]
fn drops_fenced_code_and_keeps_the_prose_around_it() {
    let text = "why does this throw?\n\n```ts\nconst x: number = \"no\";\n```\n";
    assert_eq!(derive_session_title(text), "why does this throw?");
}

#[test]
fn names_a_code_only_message_after_the_code() {
    // Nothing else is available, and an unnamed conversation is worse than one
    // named after the snippet it is about.
    assert_eq!(
        derive_session_title("```\nrg --files | wc -l\n```"),
        "rg --files | wc -l"
    );
}

#[test]
fn strips_leading_markdown_furniture() {
    assert_eq!(
        derive_session_title("## Plan the migration"),
        "Plan the migration"
    );
    assert_eq!(
        derive_session_title("- first item\n- second item"),
        "first item second item"
    );
    assert_eq!(
        derive_session_title("1. step one\n2. step two"),
        "step one step two"
    );
    assert_eq!(derive_session_title("> quoted question"), "quoted question");
}

#[test]
fn leaves_a_mid_line_hash_alone() {
    // Only *leading* furniture is markup; `#4` in a sentence is content.
    assert_eq!(
        derive_session_title("look at issue #4 again"),
        "look at issue #4 again"
    );
}

#[test]
fn cuts_on_a_word_boundary_and_keeps_the_ellipsis_inside_the_budget() {
    let text = "the quick brown fox jumps over the lazy dog and keeps on running forever";
    let title = derive_session_title_within(text, 30);
    assert!(units(&title) <= 30);
    assert!(title.ends_with('…'));
    assert_eq!(title, "the quick brown fox jumps…");
}

#[test]
fn hard_cuts_when_no_space_falls_near_the_budget() {
    let title = derive_session_title_within(&format!("short {}", "x".repeat(60)), 20);
    assert_eq!(units(&title), 20);
    assert!(title.ends_with('…'));
}

#[test]
fn never_exceeds_the_default_budget() {
    let title = derive_session_title(&"word ".repeat(200));
    assert!(units(&title) <= MAX_TITLE_CHARS);
}

#[test]
fn returns_nothing_for_a_zero_budget() {
    assert_eq!(derive_session_title_within("anything", 0), "");
}

#[test]
fn does_not_leave_a_trailing_space_before_the_ellipsis() {
    let title = derive_session_title_within("alpha beta gamma delta epsilon zeta", 18);
    assert!(!title.contains(" …"));
}

#[test]
fn budgets_in_utf16_code_units() {
    // Six dogs are twelve code units; a budget of seven keeps three and the
    // ellipsis inside it.
    let title = derive_session_title_within("🐕🐕🐕🐕🐕🐕", 7);
    assert_eq!(title, "🐕🐕🐕…");
    assert_eq!(units(&title), 7);
}

#[test]
fn ascii_digits_are_the_only_list_numbers() {
    // JavaScript's `\d` is ASCII; a line opening with other digits is content.
    assert_eq!(derive_session_title("٣. not a list"), "٣. not a list");
}
