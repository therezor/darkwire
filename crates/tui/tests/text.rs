//! Display width, cutting and folding — the invariant the frame arithmetic rests on.

use darkwire_tui::{
    STYLE_RESET, carry_styles, drop_last_grapheme, expand_controls, fit_to_width, justify,
    next_boundary, pad_to_width, previous_boundary, rule, strip_ansi, truncate_start_to_width,
    truncate_to_width, visible_width, wrap_to_width,
};
use proptest::prelude::*;

const ESC: &str = "\x1b";
const RED: &str = "\x1b[31m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
const BEL: &str = "\x07";

#[test]
fn strip_ansi_removes_sgr_and_leaves_the_text() {
    assert_eq!(strip_ansi(&format!("{RED}danger{RESET}")), "danger");
}

#[test]
fn strip_ansi_removes_cursor_movement_and_erase() {
    assert_eq!(strip_ansi(&format!("{ESC}[2A{ESC}[0Jhi")), "hi");
}

#[test]
fn strip_ansi_removes_an_osc_string_terminated_by_bel_or_st() {
    assert_eq!(strip_ansi(&format!("{ESC}]0;a title{BEL}text")), "text");
    assert_eq!(strip_ansi(&format!("{ESC}]0;a title{ESC}\\text")), "text");
}

#[test]
fn strip_ansi_removes_the_save_and_restore_a_status_bar_is_built_on() {
    // `ESC 7` and `ESC 8` are two-byte private escapes, not CSI sequences. A
    // scanner that only knows CSI counts each of them as one visible column,
    // and every width measured over such a line comes out two too wide.
    assert_eq!(strip_ansi(&format!("{ESC}7text{ESC}8")), "text");
    assert_eq!(visible_width(&format!("{ESC}7text{ESC}8")), 4);
    // Fe: the two-byte forms `ESC D` through `ESC _`.
    assert_eq!(strip_ansi(&format!("{ESC}Mtext")), "text");
}

#[test]
fn strip_ansi_leaves_an_unterminated_string_payload_visible() {
    // The introducer alone is consumed; what follows is text the operator can
    // see, rather than the rest of the line being swallowed.
    assert_eq!(strip_ansi(&format!("{ESC}]title")), "title");
    // ST is `ESC \\`; `ESC x` is not it, so the string is unterminated and the
    // stray escape stays visible rather than eating what follows.
    assert_eq!(
        strip_ansi(&format!("{ESC}_partial{ESC}x")),
        format!("partial{ESC}x")
    );
    // An unterminated CSI is not an escape at all: the bytes stay.
    assert_eq!(strip_ansi(&format!("{ESC}[12")), format!("{ESC}[12"));
    // An escape followed by something no grammar knows is two visible bytes.
    assert_eq!(strip_ansi(&format!("{ESC}ab")), format!("{ESC}ab"));
}

#[test]
fn strip_ansi_leaves_a_plain_string_untouched() {
    assert_eq!(strip_ansi("plain"), "plain");
    assert_eq!(strip_ansi(""), "");
}

#[test]
fn visible_width_counts_ascii_as_its_length() {
    assert_eq!(visible_width("hello"), 5);
}

#[test]
fn visible_width_charges_nothing_for_escape_sequences() {
    // The reason this module exists: the byte count here is 15, and a menu that
    // believed it would truncate a five-column row to nothing.
    assert_eq!(visible_width(&format!("{RED}hello{RESET}")), 5);
}

#[test]
fn visible_width_counts_cjk_as_two_columns() {
    assert_eq!(visible_width("日本語"), 6);
    assert_eq!(visible_width("한글"), 4);
}

#[test]
fn visible_width_counts_an_emoji_as_two_with_or_without_the_selector() {
    assert_eq!(visible_width("🚀"), 2);
    assert_eq!(visible_width("✅"), 2);
    // U+2714 is one column as text and two with the emoji presentation selector.
    assert_eq!(visible_width("✔"), 1);
    assert_eq!(visible_width("✔\u{fe0f}"), 2);
}

#[test]
fn visible_width_counts_a_zwj_sequence_once() {
    // One grapheme cluster, drawn in two columns, spelled with five code points.
    let family = "👨\u{200d}👩\u{200d}👧";
    assert!(family.chars().count() > 2);
    assert_eq!(visible_width(family), 2);
}

#[test]
fn visible_width_charges_nothing_for_a_combining_mark() {
    assert_eq!(visible_width("é"), 1);
    assert_eq!(visible_width("e\u{301}"), 1);
    assert_eq!(visible_width("\u{301}"), 0);
}

#[test]
fn visible_width_counts_an_empty_string_as_zero() {
    assert_eq!(visible_width(""), 0);
}

#[test]
fn truncate_returns_the_string_unchanged_when_it_fits() {
    assert_eq!(truncate_to_width("hello", 10, "…"), "hello");
    assert_eq!(truncate_to_width("hello", 5, "…"), "hello");
}

#[test]
fn truncate_includes_the_ellipsis_in_the_budget() {
    // One column over budget is a line that wraps, and a wrapped line is a row
    // the renderer's erase never reaches.
    let cut = truncate_to_width("abcdefghij", 5, "…");
    assert!(visible_width(&cut) <= 5);
    assert_eq!(cut, "abcd…");
}

#[test]
fn truncate_never_cuts_inside_an_escape_sequence() {
    let cut = truncate_to_width(&format!("{RED}abcdefghij{RESET}"), 5, "…");
    assert_eq!(strip_ansi(&cut), "abcd…");
    assert!(cut.starts_with(RED));
}

#[test]
fn truncate_closes_an_open_attribute_when_it_cuts() {
    let cut = truncate_to_width(&format!("{RED}abcdefghij"), 5, "…");
    assert!(cut.ends_with(RESET));
}

#[test]
fn truncate_does_not_append_a_reset_when_nothing_was_left_open() {
    let cut = truncate_to_width(&format!("{RED}abc{RESET}defghij"), 5, "…");
    assert!(!cut.ends_with(RESET));
}

#[test]
fn truncate_never_splits_a_wide_character() {
    // Two columns will not fit in one, so the character is dropped whole.
    assert!(visible_width(&truncate_to_width("日本語", 3, "…")) <= 3);
}

#[test]
fn truncate_returns_nothing_for_a_width_of_zero() {
    assert_eq!(truncate_to_width("hello", 0, "…"), "");
}

#[test]
fn truncate_drops_the_ellipsis_rather_than_exceed_a_width_too_small_for_it() {
    assert!(visible_width(&truncate_to_width("日本語", 1, "…")) <= 1);
    assert_eq!(truncate_to_width("abcdef", 1, "..."), "a");
}

#[test]
fn truncate_accepts_a_different_ellipsis() {
    assert_eq!(truncate_to_width("abcdefghij", 6, ".."), "abcd..");
}

#[test]
fn truncate_start_keeps_the_end() {
    assert_eq!(truncate_start_to_width("abcdefghij", 5, "…"), "…ghij");
}

#[test]
fn truncate_start_leaves_a_string_that_fits_alone() {
    assert_eq!(truncate_start_to_width("abc", 5, "…"), "abc");
}

#[test]
fn truncate_start_never_returns_more_columns_than_asked_for() {
    for text in ["abcdefghij", "日本語のラベル", "a🚀b🚀c🚀d"] {
        for max in [1, 2, 3, 5, 8] {
            assert!(visible_width(&truncate_start_to_width(text, max, "…")) <= max);
        }
    }
}

#[test]
fn truncate_start_is_nothing_for_no_width() {
    assert_eq!(truncate_start_to_width("abc", 0, "…"), "");
}

#[test]
fn pad_pads_a_short_string_and_leaves_a_long_one_alone() {
    assert_eq!(pad_to_width("ab", 5), "ab   ");
    assert_eq!(pad_to_width("abcdef", 3), "abcdef");
}

#[test]
fn pad_pads_by_columns_rather_than_bytes() {
    assert_eq!(
        visible_width(&pad_to_width(&format!("{RED}ab{RESET}"), 5)),
        5
    );
    assert_eq!(visible_width(&pad_to_width("日", 5)), 5);
}

#[test]
fn fit_produces_exactly_the_requested_width_either_way() {
    assert_eq!(visible_width(&fit_to_width("ab", 6)), 6);
    assert_eq!(visible_width(&fit_to_width("abcdefghij", 6)), 6);
}

#[test]
fn drop_last_grapheme_removes_what_a_person_can_see() {
    assert_eq!(drop_last_grapheme("ab"), "a");
    assert_eq!(drop_last_grapheme("a🚀"), "a");
    assert_eq!(drop_last_grapheme("a👨\u{200d}👩\u{200d}👧"), "a");
    assert_eq!(drop_last_grapheme("ae\u{301}"), "a");
}

#[test]
fn drop_last_grapheme_has_nothing_to_remove_from_nothing() {
    assert_eq!(drop_last_grapheme(""), "");
}

#[test]
fn boundaries_step_by_grapheme_cluster() {
    let text = "a👨\u{200d}👩\u{200d}👧b";
    let family_end = text.len() - 1;
    assert_eq!(previous_boundary(text, text.len()), family_end);
    assert_eq!(previous_boundary(text, family_end), 1);
    assert_eq!(previous_boundary(text, 1), 0);
    assert_eq!(previous_boundary(text, 0), 0);
    assert_eq!(next_boundary(text, 0), 1);
    assert_eq!(next_boundary(text, 1), family_end);
    assert_eq!(next_boundary(text, family_end), text.len());
    assert_eq!(next_boundary(text, text.len()), text.len());
    assert_eq!(next_boundary("", 0), 0);
}

#[test]
fn justify_pushes_the_two_halves_to_opposite_ends() {
    assert_eq!(justify("left", "right", 20), "left           right");
}

#[test]
fn justify_measures_the_gap_in_columns() {
    assert_eq!(
        visible_width(&justify(&format!("{RED}left{RESET}"), "right", 20)),
        20
    );
    assert_eq!(visible_width(&justify("日本語", "right", 20)), 20);
}

#[test]
fn justify_keeps_the_right_hand_side_whole_and_truncates_the_left() {
    // The right end is the model and the context budget — the fields that
    // change. A bar that dropped them to keep a workspace name would be
    // showing the part nobody is watching.
    let line = justify("a-very-long-workspace-name", "ollama/qwen3", 20);
    assert!(line.contains("ollama/qwen3"));
    assert!(visible_width(&line) <= 20);
}

#[test]
fn justify_leaves_at_least_one_column_between_them() {
    assert!(justify("abcdefgh", "right", 14).contains(" right"));
}

#[test]
fn justify_gives_the_width_to_the_right_when_both_cannot_fit() {
    assert!(visible_width(&justify("left", "right", 4)) <= 4);
    assert_eq!(justify("left", "right", 0), "");
}

#[test]
fn rule_is_exactly_the_width_asked_for() {
    assert_eq!(visible_width(&rule(10, "─")), 10);
    assert_eq!(rule(3, "─"), "───");
    assert_eq!(rule(3, "="), "===");
    assert_eq!(rule(0, "─"), "");
}

#[test]
fn carry_styles_keeps_what_is_open_and_drops_it_on_a_reset() {
    assert_eq!(carry_styles("", &format!("{RED}text")), RED);
    assert_eq!(
        carry_styles(RED, &format!("{BOLD}more")),
        format!("{RED}{BOLD}")
    );
    assert_eq!(carry_styles(RED, &format!("text{RESET}")), "");
    assert_eq!(carry_styles(RED, &format!("text{ESC}[m")), "");
    // A non-SGR escape closes the carry too: it is not a style.
    assert_eq!(carry_styles(RED, &format!("{ESC}[2K")), "");
    assert_eq!(STYLE_RESET, RESET);
}

#[test]
fn the_escapes_that_cost_no_columns_include_apc_strings() {
    // The cursor marker is one. Matching only the two-byte introducer left
    // `darkwire:cursor` behind as visible text and measured the marker as
    // fifteen columns, which folded the editor's line fifteen columns early.
    // Both terminators: ST is what the marker uses and what the standard says,
    // BEL is the xterm extension a stray sequence may well arrive with.
    for apc in [
        format!("{ESC}_darkwire:cursor{ESC}\\"),
        format!("{ESC}_darkwire:cursor{BEL}"),
    ] {
        assert_eq!(visible_width(&apc), 0);
        assert_eq!(strip_ansi(&format!("a{apc}b")), "ab");
    }
}

#[test]
fn a_hyperlink_measures_as_its_text_and_nothing_else() {
    let osc = format!("{ESC}]8;;https://example.com{BEL}");
    assert_eq!(visible_width(&format!("{osc}link")), 4);
}

#[test]
fn wrap_breaks_at_a_space_rather_than_mid_word() {
    assert_eq!(wrap_to_width("one two three", 8), ["one two", "three"]);
}

#[test]
fn wrap_breaks_inside_a_word_that_has_no_space_to_break_at() {
    // A URL or a hash longer than the window still has to be shown.
    assert_eq!(wrap_to_width("abcdefghij", 4), ["abcd", "efgh", "ij"]);
}

#[test]
fn wrap_hands_back_the_line_untouched_when_it_fits() {
    assert_eq!(wrap_to_width("short", 40), ["short"]);
    assert_eq!(wrap_to_width("anything at all", 0), ["anything at all"]);
}

#[test]
fn wrap_lets_a_cluster_wider_than_the_window_overhang() {
    // Two columns will never fit in one; folding forever onto empty rows is
    // the alternative, and it is worse.
    assert_eq!(wrap_to_width("日本", 1), ["日", "本"]);
}

#[test]
fn wrap_carries_an_open_colour_across_the_fold() {
    // A colour that stopped at the fold would be a colour that changed with
    // the window size.
    let rows = wrap_to_width(&format!("{RED}one two three"), 8);
    assert!(rows[1].starts_with(RED));
    // And a reset before the fold means nothing is carried.
    let rows = wrap_to_width(&format!("{RED}one{RESET} two three"), 8);
    assert!(!rows[1].starts_with(RED));
}

proptest! {
    // A column table is exactly the kind of code where a hand-picked example
    // passes and the next locale does not, so the invariants the frame
    // actually depends on are asserted over generated input rather than samples.

    #[test]
    fn measures_a_stripped_string_the_same_as_the_styled_one(text in "\\PC{0,40}") {
        let styled = format!("{BOLD}{text}{RESET}");
        prop_assert_eq!(visible_width(&styled), visible_width(&strip_ansi(&styled)));
    }

    #[test]
    fn never_returns_more_columns_than_asked_for(text in "\\PC{0,40}", max in 1_usize..=40) {
        prop_assert!(visible_width(&truncate_to_width(&text, max, "…")) <= max);
    }

    #[test]
    fn pads_to_exactly_the_width_whenever_the_text_fits(text in "\\PC{0,40}", width in 0_usize..=40) {
        prop_assume!(visible_width(&text) <= width);
        prop_assert_eq!(visible_width(&pad_to_width(&text, width)), width);
    }

    #[test]
    fn wrap_never_draws_a_row_wider_than_the_width(text in "[ -~]{0,60}", width in 1_usize..=40) {
        for row in wrap_to_width(&text, width) {
            prop_assert!(visible_width(&row) <= width);
        }
    }

    #[test]
    fn wrap_keeps_every_visible_character(text in "[a-z ]{0,60}", width in 2_usize..=20) {
        let rows = wrap_to_width(&text, width);
        // Spaces at a fold are what the fold replaces, so compare without them.
        prop_assert_eq!(rows.concat().replace(' ', ""), text.replace(' ', ""));
    }
}

#[test]
fn a_tab_becomes_the_spaces_to_the_next_stop() {
    assert_eq!(expand_controls("a\tb", 0), "a   b");
    assert_eq!(expand_controls("\tb", 0), "    b");
    // A chunk that starts mid-line counts from where the line had got to.
    assert_eq!(expand_controls("\tb", 2), "  b");
    // A newline starts the count again.
    assert_eq!(expand_controls("abc\n\tb", 0), "abc\n    b");
}

#[test]
fn a_tab_after_a_wide_glyph_counts_its_two_columns() {
    assert_eq!(expand_controls("你\tb", 0), "你  b");
}

#[test]
fn other_control_characters_are_dropped() {
    assert_eq!(
        expand_controls("a\u{7}b\rc\u{0}d\u{7f}e\u{85}f", 0),
        "abcdef"
    );
    assert_eq!(expand_controls("line\r\nnext", 0), "line\nnext");
}

#[test]
fn escape_sequences_survive_and_a_lone_escape_does_not() {
    assert_eq!(
        expand_controls(&format!("{RED}\tx{RESET}"), 0),
        format!("{RED}    x{RESET}")
    );
    assert_eq!(expand_controls(&format!("a{ESC}"), 0), "a");
}
