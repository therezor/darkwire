//! The startup block and the status bar.
//!
//! Both are functions over one plain record, so every case here is about text
//! and none of them boots anything. The width assertions are the ones worth
//! keeping: a row that wraps takes two of the rows the bar reserved, and the
//! one it pushes off the bottom is the one the operator was reading.

// `allow-unwrap-in-tests` covers a `#[test]` body; the fixture helpers beside
// them are ordinary functions, and a helper that returned `Result` would make
// every case carry a `?` for a temporary directory that cannot fail in a way
// the case could act on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "fixture helpers in an integration test"
)]

use darkwire::header::{
    ContextUsage, HeaderView, context_label, input_rule, startup_header, status_bar,
};
use darkwire::i18n::Translations;
use darkwire_tui::{PLAIN_THEME, visible_width};

fn view() -> HeaderView {
    HeaderView {
        agent: "Reviewer".to_owned(),
        model: "claude-opus-5".to_owned(),
        provider: "Anthropic".to_owned(),
        workspaces: "/home/dev/DarkWire/workspaces".to_owned(),
        workspace_name: "Research".to_owned(),
        session: "a conversation".to_owned(),
        session_key: "cli-9f2ab1".to_owned(),
        context: Some(ContextUsage {
            used_tokens: 15_872,
            window_tokens: 128_000,
        }),
    }
}

#[test]
fn names_every_field_the_operator_needs_to_know_they_are_in_the_right_place() {
    let t = Translations::default();
    let header = startup_header(&view(), 80, &PLAIN_THEME, &t, false);
    for value in [
        "Reviewer",
        "claude-opus-5",
        "Anthropic",
        "/home/dev/DarkWire/workspaces",
        "a conversation",
    ] {
        assert!(header.contains(value), "missing {value} in:\n{header}");
    }
}

#[test]
fn aligns_the_values_in_one_column_measured_rather_than_counted() {
    // A run of hand-counted spaces holds only until the first translation is
    // longer than the English it replaced, and then it is wrong for every row.
    let t = Translations::default();
    let header = startup_header(&view(), 80, &PLAIN_THEME, &t, false);
    let rows: Vec<&str> = header
        .lines()
        .filter(|line| line.contains("claude-opus-5") || line.contains("Anthropic"))
        .collect();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].find("claude-opus-5"), rows[1].find("Anthropic"));
}

#[test]
fn mentions_the_menu_only_when_there_is_a_terminal_that_can_draw_one() {
    let t = Translations::default();
    assert!(startup_header(&view(), 80, &PLAIN_THEME, &t, true).contains("ctrl-g"));
    assert!(!startup_header(&view(), 80, &PLAIN_THEME, &t, false).contains("ctrl-g"));
}

#[test]
fn never_writes_a_line_wider_than_the_window() {
    let t = Translations::default();
    for width in [20, 40, 80] {
        for line in startup_header(&view(), width, &PLAIN_THEME, &t, true).lines() {
            assert!(
                visible_width(line) <= width,
                "{width}: {line:?} is {} wide",
                visible_width(line)
            );
        }
    }
}

#[test]
fn context_label_says_how_full_the_window_is_and_how_big_it_is() {
    assert_eq!(
        context_label(Some(&ContextUsage {
            used_tokens: 15_872,
            window_tokens: 128_000,
        })),
        "12.4%/128k"
    );
}

#[test]
fn context_label_says_nothing_before_a_turn_has_been_measured() {
    assert_eq!(context_label(None), "");
}

#[test]
fn context_label_says_nothing_rather_than_dividing_by_a_window_of_nothing() {
    assert_eq!(
        context_label(Some(&ContextUsage {
            used_tokens: 10,
            window_tokens: 0,
        })),
        ""
    );
}

#[test]
fn context_label_rounds_the_window_to_the_nearest_thousand() {
    // Halves go up, which is what a reader expects of a rounded budget.
    assert_eq!(
        context_label(Some(&ContextUsage {
            used_tokens: 0,
            window_tokens: 1_500,
        })),
        "0.0%/2k"
    );
    assert_eq!(
        context_label(Some(&ContextUsage {
            used_tokens: 0,
            window_tokens: 1_499,
        })),
        "0.0%/1k"
    );
}

#[test]
fn status_bar_opens_with_a_rule_the_width_it_was_given() {
    // The whole frame is here rather than half of it in the prompt: a prompt
    // string is written into the scrollback, so a rule drawn there outlives the
    // turn and becomes a separator between messages.
    let bar = status_bar(&view(), 40, &PLAIN_THEME);
    assert_eq!(visible_width(&bar[0]), 40);
    assert!(bar[0].chars().all(|glyph| glyph == '─'));
}

#[test]
fn status_bar_puts_the_workspace_and_the_agent_at_opposite_ends() {
    let bar = status_bar(&view(), 40, &PLAIN_THEME);
    assert!(bar[1].starts_with("Research"));
    assert!(bar[1].ends_with("Reviewer"));
}

#[test]
fn status_bar_puts_the_context_budget_and_the_model_on_the_next_row() {
    let bar = status_bar(&view(), 44, &PLAIN_THEME);
    assert!(bar[2].starts_with("12.4%/128k"));
    assert!(bar[2].ends_with("Anthropic/claude-opus-5"));
}

#[test]
fn status_bar_names_the_model_alone_when_there_is_no_provider_to_name() {
    let bar = status_bar(
        &HeaderView {
            provider: String::new(),
            ..view()
        },
        44,
        &PLAIN_THEME,
    );
    assert!(bar[2].ends_with("claude-opus-5"));
    assert!(!bar[2].contains("/claude"));
}

#[test]
fn status_bar_leaves_the_context_side_blank_before_a_turn_has_been_measured() {
    let bar = status_bar(
        &HeaderView {
            context: None,
            ..view()
        },
        44,
        &PLAIN_THEME,
    );
    assert!(bar[2].trim_start().starts_with("Anthropic"));
}

#[test]
fn status_bar_never_writes_a_row_wider_than_the_window() {
    for width in [12, 24, 40, 100] {
        for row in status_bar(&view(), width, &PLAIN_THEME) {
            assert!(visible_width(&row) <= width, "{width}: {row:?}");
        }
    }
}

#[test]
fn status_bar_is_three_rows_which_is_what_the_caller_has_to_reserve() {
    assert_eq!(status_bar(&view(), 40, &PLAIN_THEME).len(), 3);
}

#[test]
fn input_rule_matches_the_one_under_the_editor() {
    // Both halves of the frame are the same line, measured the same way.
    let bar = status_bar(&view(), 37, &PLAIN_THEME);
    assert_eq!(input_rule(37, &PLAIN_THEME), bar[0]);
}

#[test]
fn the_name_is_drawn_when_the_window_can_hold_it() {
    let header = startup_header(&view(), 80, &PLAIN_THEME, &Translations::default(), false);
    let rows: Vec<&str> = header.lines().collect();
    assert!(rows[0].contains("┌┬┐"), "{rows:?}");
    assert!(rows[2].contains("─┴┘"), "{rows:?}");
    // Three rows of it and no more; the rest of the block is the install.
    assert!(rows[3].contains("agent"), "{rows:?}");
}

#[test]
fn a_narrow_window_gets_the_name_written_rather_than_cut() {
    // A wordmark cut in half is unreadable in a way a plain word never is, and
    // the window that cannot hold it is the one where every row is worth more.
    let header = startup_header(&view(), 20, &PLAIN_THEME, &Translations::default(), false);
    let rows: Vec<&str> = header.lines().collect();
    assert_eq!(rows[0], "  darkwire");
    assert!(!header.contains("┌┬┐"), "{header}");
}

#[test]
fn every_row_of_the_drawn_name_is_the_same_width() {
    // They are one picture. A row a column short leans.
    let widths: Vec<usize> = [
        "┌┬┐┌─┐┬─┐┬┌─┬ ┬┬┬─┐┌─┐",
        " ││├─┤├┬┘├┴┐││││├┬┘├┤ ",
        "─┴┘┴ ┴┴└─┴ ┴└┴┘┴┴└─└─┘",
    ]
    .iter()
    .map(|row| visible_width(row))
    .collect();
    assert_eq!(widths, [22, 22, 22]);
}

#[test]
fn the_startup_block_names_the_session_key_as_well_as_its_title() {
    // The title is what a person recognises; the key is what `/session <key>`,
    // `--session` and the REST path take. Printing only the first means
    // running a command to find out where you already are.
    let t = Translations::default();
    let block = startup_header(&view(), 80, &PLAIN_THEME, &t, true);

    assert!(block.contains("a conversation"), "{block}");
    assert!(block.contains("cli-9f2ab1"), "{block}");
}
