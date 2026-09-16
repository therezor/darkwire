//! Escaping, chunking and the two Markdown constructs that survive.

use darkwire_channels::telegram::format::{
    MAX_MESSAGE_CHARS, chunk_message, escape_markdown_v2, strip_markdown, to_markdown_v2,
};
use proptest::prelude::*;

/// The eighteen MarkdownV2 reserves, plus the backslash that escapes them.
const RESERVED: &str = r"_*[]()~`>#+-=|{}.!\";

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

// escape_markdown_v2

#[test]
fn escapes_every_reserved_character() {
    let expected: String = RESERVED.chars().flat_map(|c| ['\\', c]).collect();

    assert_eq!(escape_markdown_v2(RESERVED), expected);
}

#[test]
fn leaves_ordinary_prose_alone() {
    assert_eq!(escape_markdown_v2("hello world"), "hello world");
}

#[test]
fn escapes_the_full_stop_that_ends_a_sentence() {
    // The single commonest way to earn a `can't parse entities` 400.
    assert_eq!(escape_markdown_v2("Done."), r"Done\.");
}

// to_markdown_v2

#[test]
fn passes_a_fenced_block_through_with_only_backticks_escaped() {
    let out = to_markdown_v2("```ts\nconst a = b.c;\n```");

    // The `.` inside code is untouched: escaping it would render as `b\.c`.
    assert_eq!(out, "```ts\nconst a = b.c;\n```");
}

#[test]
fn escapes_prose_around_a_fenced_block() {
    let out = to_markdown_v2("Here.\n```\nx = 1.0\n```\nDone.");

    assert!(out.contains(r"Here\."), "{out}");
    assert!(out.contains("x = 1.0"), "{out}");
    assert!(out.contains(r"Done\."), "{out}");
}

#[test]
fn keeps_inline_code_as_code() {
    assert_eq!(
        to_markdown_v2("run `npm i -D x` now"),
        "run `npm i -D x` now"
    );
}

#[test]
fn escapes_a_backtick_inside_inline_code() {
    assert_eq!(to_markdown_v2(r"`a\b`"), r"`a\\b`");
}

#[test]
fn rewrites_double_asterisk_bold_into_the_single_asterisk_form() {
    assert_eq!(to_markdown_v2("a **bold** word"), "a *bold* word");
}

#[test]
fn keeps_underscore_italics() {
    assert_eq!(to_markdown_v2("an _italic_ word"), "an _italic_ word");
}

#[test]
fn escapes_an_unpaired_asterisk_rather_than_opening_an_entity() {
    // A half-finished sentence from a truncated turn must still send.
    assert_eq!(to_markdown_v2("2 ** 8"), r"2 \*\* 8");
}

#[test]
fn does_not_treat_a_snake_case_identifier_as_italics() {
    assert_eq!(
        to_markdown_v2("read_file and write_file"),
        r"read\_file and write\_file"
    );
}

#[test]
fn does_not_open_an_italic_on_a_doubled_underscore() {
    // `__x__` has no partner under the rule: the second `_` is preceded by a
    // word character and the first is followed by one.
    assert_eq!(to_markdown_v2("__x__"), r"\_\_x\_\_");
}

#[test]
fn does_not_italicise_across_a_line_break() {
    assert_eq!(to_markdown_v2("_a\nb_"), "\\_a\nb\\_");
}

#[test]
fn leaves_an_escaped_underscore_alone() {
    // A `\` before the opener is the one thing that disqualifies it.
    assert!(!to_markdown_v2(r"\_a_").contains('\u{1}'));
}

#[test]
fn treats_an_unterminated_fence_as_prose_rather_than_failing() {
    // A turn that hit its iteration limit mid-code-block produces exactly this.
    let out = to_markdown_v2("```ts\nconst a = 1.");

    assert!(out.contains(r"\."), "{out}");
    assert!(out.contains(r"\`\`\`"), "{out}");
}

#[test]
fn leaves_an_empty_message_empty() {
    assert_eq!(to_markdown_v2(""), "");
}

#[test]
fn escapes_the_info_string_of_a_fence() {
    let out = to_markdown_v2("```c.d\nx\n```");

    assert!(out.starts_with("```c\\.d\n"), "{out}");
}

// chunk_message

#[test]
fn leaves_a_message_that_fits_as_one_piece() {
    assert_eq!(chunk_message("short", MAX_MESSAGE_CHARS), vec!["short"]);
}

#[test]
fn cuts_on_a_line_boundary_rather_than_mid_line() {
    let chunks = chunk_message("aaaa\nbbbb\ncccc", 10);

    assert!(chunks.len() > 1);
    for chunk in &chunks {
        assert!(utf16_len(chunk) <= 10, "{chunk:?}");
    }
}

#[test]
fn never_ends_a_piece_on_a_dangling_backslash() {
    // Half an escape sequence sends a stray backslash and leaves the character
    // it was protecting unescaped at the head of the next message.
    let line = r"\.".repeat(40);
    let chunks = chunk_message(&line, 11);

    assert!(chunks.len() > 1);
    for chunk in &chunks {
        assert!(!chunk.ends_with('\\'), "{chunk:?}");
    }
}

#[test]
fn closes_and_reopens_a_fence_that_spans_a_cut() {
    let body: Vec<String> = (0..20).map(|index| format!("line {index}")).collect();
    let chunks = chunk_message(&format!("```\n{}\n```", body.join("\n")), 60);

    assert!(chunks.len() > 1);
    // Every piece carries an even number of fence markers, which is what makes
    // it valid on its own.
    for chunk in &chunks {
        assert_eq!(chunk.matches("```").count() % 2, 0, "{chunk:?}");
    }
}

#[test]
fn does_not_reopen_a_fence_that_closed_on_the_boundary() {
    let chunks = chunk_message("```\nabc\n```\n\nthen prose here", 16);

    assert!(!chunks.last().expect("a chunk").contains("```"));
}

#[test]
fn returns_one_empty_piece_for_empty_input() {
    // A caller that is sending a message must get something to send.
    assert_eq!(chunk_message("", MAX_MESSAGE_CHARS), vec![String::new()]);
}

#[test]
fn uses_telegrams_own_ceiling() {
    assert_eq!(MAX_MESSAGE_CHARS, 4096);
    assert_eq!(chunk_message(&"x".repeat(5000), MAX_MESSAGE_CHARS).len(), 2);
}

#[test]
fn counts_an_astral_character_as_two_units_and_never_splits_it() {
    // Telegram counts UTF-16, so ten emoji are twenty units, not ten.
    let chunks = chunk_message(&"😀".repeat(10), 8);

    assert!(chunks.len() > 1);
    for chunk in &chunks {
        assert!(utf16_len(chunk) <= 8, "{chunk:?}");
        assert_eq!(chunk.chars().count() * 2, utf16_len(chunk));
    }
}

// strip_markdown

#[test]
fn strip_markdown_undoes_escaping() {
    assert_eq!(strip_markdown(&escape_markdown_v2(RESERVED)), RESERVED);
}

#[test]
fn strip_markdown_leaves_a_backslash_before_an_ordinary_character() {
    assert_eq!(strip_markdown(r"\n and \."), r"\n and .");
}

// Properties

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// The one guarantee a caller has: nothing it is handed is refused for
    /// length.
    #[test]
    fn never_exceeds_the_limit(text in ".{0,2000}") {
        for chunk in chunk_message(&text, 40) {
            prop_assert!(utf16_len(&chunk) <= 40, "{chunk:?}");
        }
    }

    /// Reassembly is lossy by exactly the separators and the synthetic fences,
    /// so the property is about characters surviving rather than a round trip.
    #[test]
    fn keeps_every_non_newline_character(text in "[^`]{0,500}") {
        let joined = chunk_message(&text, 40).join("\n");
        prop_assert_eq!(joined.replace('\n', ""), text.replace('\n', ""));
    }

    /// A message that will not send is worse than one that renders plainly, so
    /// the formatter must terminate and produce something for any input.
    #[test]
    fn formatting_any_text_terminates(text in ".{0,500}") {
        let formatted = to_markdown_v2(&text);
        prop_assert!(formatted.len() >= text.len() || text.contains("```"));
    }
}
