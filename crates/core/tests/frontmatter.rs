//! The frontmatter parser.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::collections::BTreeMap;

use darkwire_core::frontmatter::parse_frontmatter;

fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn splits_the_fenced_block_from_the_body() {
    let parsed = parse_frontmatter(
        &[
            "---",
            "name: review",
            "description: Read a diff.",
            "---",
            "",
            "Body.",
        ]
        .join("\n"),
    );
    assert_eq!(
        parsed.fields,
        fields(&[("name", "review"), ("description", "Read a diff.")])
    );
    assert_eq!(parsed.body, "Body.");
}

#[test]
fn treats_a_file_with_no_fence_as_all_body() {
    let parsed = parse_frontmatter("# Just markdown\n\nNo fence here.");
    assert!(parsed.fields.is_empty());
    assert_eq!(parsed.body, "# Just markdown\n\nNo fence here.");
}

#[test]
fn treats_an_unterminated_fence_as_all_body() {
    // The alternative reading, everything after the opening fence is
    // frontmatter, turns one missing line into a skill with no instructions.
    let parsed = parse_frontmatter("---\ndescription: Oops.\n\nThe body.");
    assert!(parsed.fields.is_empty());
    assert!(parsed.body.contains("The body."));
}

#[test]
fn finds_the_closing_fence_through_crlf_line_endings() {
    let parsed = parse_frontmatter("---\r\ndescription: A.\r\n---\r\nBody.");
    assert_eq!(parsed.fields["description"], "A.");
    assert_eq!(parsed.body, "Body.");
}

#[test]
fn strips_one_matching_pair_of_quotes_and_only_a_matching_pair() {
    let parsed = parse_frontmatter(
        &[
            "---",
            "a: \"quoted\"",
            "b: 'single'",
            "c: \"unbalanced",
            "---",
            "",
        ]
        .join("\n"),
    );
    assert_eq!(
        parsed.fields,
        fields(&[("a", "quoted"), ("b", "single"), ("c", "\"unbalanced")])
    );
}

#[test]
fn leaves_a_lone_quote_alone_rather_than_deleting_it() {
    // A one-character value cannot be a quoted pair, and stripping it would
    // turn a typo into an empty field.
    assert_eq!(parse_frontmatter("---\na: \"\n---\n").fields["a"], "\"");
}

#[test]
fn keeps_a_colon_inside_a_value() {
    let parsed = parse_frontmatter("---\ndescription: Use it: it is good.\n---\n");
    assert_eq!(parsed.fields["description"], "Use it: it is good.");
}

#[test]
fn skips_blank_lines_comments_and_anything_that_is_not_key_value() {
    // Skipped rather than refused: a `tags:` list nobody reads is not a reason
    // to refuse the two fields that are read.
    let parsed = parse_frontmatter(
        &[
            "---",
            "",
            "# a comment",
            "tags:",
            "  - one",
            "description: Kept.",
            "---",
            "",
        ]
        .join("\n"),
    );
    assert_eq!(
        parsed.fields,
        fields(&[("tags", ""), ("description", "Kept.")])
    );
}

#[test]
fn flattens_one_level_of_nesting_to_a_dotted_key() {
    // What a memory file's `metadata.type` rides on.
    let parsed =
        parse_frontmatter(&["---", "name: x", "metadata:", "  type: user", "---", ""].join("\n"));
    assert_eq!(
        parsed.fields,
        fields(&[("name", "x"), ("metadata", ""), ("metadata.type", "user")])
    );
}

#[test]
fn does_not_let_a_nested_key_shadow_a_real_one() {
    // The hazard the nesting rule was added to close.
    let parsed = parse_frontmatter(
        &[
            "---",
            "name: real",
            "metadata:",
            "  name: nested",
            "---",
            "",
        ]
        .join("\n"),
    );
    assert_eq!(parsed.fields["name"], "real");
    assert_eq!(parsed.fields["metadata.name"], "nested");
}

#[test]
fn does_not_hang_an_indented_line_off_a_key_that_has_a_value() {
    let parsed =
        parse_frontmatter(&["---", "name: Deploy", "  stray: value", "---", ""].join("\n"));
    assert_eq!(parsed.fields, fields(&[("name", "Deploy")]));
}

#[test]
fn lets_a_repeated_key_win_last() {
    assert_eq!(
        parse_frontmatter("---\na: first\na: second\n---\n").fields["a"],
        "second"
    );
}

#[test]
fn reads_an_empty_value_as_empty_rather_than_dropping_the_key() {
    assert_eq!(
        parse_frontmatter("---\ndescription:\n---\nBody.").fields["description"],
        ""
    );
}

#[test]
fn a_byte_order_mark_does_not_hide_the_opening_fence() {
    let parsed = parse_frontmatter("\u{FEFF}---\nname: x\n---\nBody.");
    assert_eq!(parsed.fields["name"], "x");
    assert_eq!(parsed.body, "Body.");
}

#[test]
fn a_stray_line_clears_the_parent_so_the_next_indent_is_not_nested() {
    let parsed = parse_frontmatter(
        &[
            "---",
            "metadata:",
            "  - list item",
            "  type: user",
            "---",
            "",
        ]
        .join("\n"),
    );
    assert_eq!(parsed.fields, fields(&[("metadata", "")]));
}
