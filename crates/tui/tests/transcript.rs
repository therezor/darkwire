//! Fragments become logical lines, refolded per width, styles carried across breaks, bounded.

use darkwire_tui::{Component, Transcript};

const ESC: &str = "\x1b";
const DIM: &str = "\x1b[2m";
const OFF: &str = "\x1b[22m";

#[test]
fn joins_fragments_that_do_not_end_on_a_line_break() {
    // Which is how a streamed answer arrives: `the par`, `ser is`, `\n`.
    let mut transcript = Transcript::new();
    transcript.write("the par");
    transcript.write("ser is");
    transcript.write(" split\n");

    assert_eq!(transcript.lines(), ["the parser is split", ""]);
}

#[test]
fn reports_whether_the_last_thing_written_ended_a_line() {
    let mut transcript = Transcript::new();
    assert!(transcript.at_line_start());

    transcript.write("half");
    assert!(!transcript.at_line_start());

    transcript.write("\n");
    assert!(transcript.at_line_start());

    // An empty write changes nothing.
    transcript.write("");
    assert!(transcript.at_line_start());
    assert_eq!(transcript.lines(), ["half", ""]);
}

#[test]
fn refolds_at_the_width_it_is_asked_for_rather_than_the_one_it_took() {
    // The whole reason the conversation is kept as text: a narrower window
    // re-wraps the paragraph instead of clipping it.
    let mut transcript = Transcript::new();
    transcript.write("one two three four five six\n");

    assert_eq!(transcript.render(40), ["one two three four five six", ""]);
    assert_eq!(
        transcript.render(12),
        ["one two", "three four", "five six", ""]
    );
}

#[test]
fn hands_back_the_same_rows_between_writes() {
    // The renderer's diff compares row against row on every keystroke; the
    // rows have to be the same ones, not a re-wrap that happens to agree.
    let mut transcript = Transcript::new();
    transcript.write("a line\n");

    let first = transcript.render(40);
    assert_eq!(transcript.render(40), first);
}

#[test]
fn rebuilds_after_a_write() {
    let mut transcript = Transcript::new();
    transcript.write("first\n");
    let before = transcript.render(40);
    transcript.write("second\n");

    assert_ne!(transcript.render(40), before);
    assert_eq!(transcript.render(40), ["first", "second", ""]);
}

#[test]
fn clear_forgets_everything_including_an_open_style() {
    let mut transcript = Transcript::new();
    transcript.write(&format!("{DIM}open"));
    transcript.render(40);
    transcript.clear();

    assert!(transcript.lines().is_empty());
    assert!(transcript.at_line_start());
    assert!(transcript.render(40).is_empty());

    transcript.write("plain\n");
    assert_eq!(transcript.lines(), ["plain", ""]);
}

#[test]
fn a_style_that_spans_a_line_break_is_reopened_below() {
    // The shape a streamed chunk of reasoning actually has: `"\n\nLet me"`,
    // dimmed whole. The opener lands on the line above, which the terminal has
    // already drawn, so the first words of the paragraph rendered plain white
    // against dim grey — "the first letter of a sentence is not grey".
    let mut transcript = Transcript::new();
    transcript.write(&format!("{DIM}tools.{OFF}"));
    transcript.write(&format!("{DIM}\n\nLet me think{OFF}"));

    let rows = transcript.render(80);
    let last = rows.last().unwrap();
    assert!(last.starts_with(DIM));
    assert!(last.contains("Let me think"));
}

#[test]
fn the_line_a_style_leaves_is_closed_so_it_does_not_bleed_downward() {
    let mut transcript = Transcript::new();
    transcript.write(&format!("{DIM}open"));
    transcript.write("\nplain");

    assert!(transcript.lines()[0].ends_with(&format!("{ESC}[0m")));
    assert!(transcript.lines()[1].starts_with(DIM));
}

#[test]
fn still_reports_the_start_of_a_line_when_the_style_is_all_that_is_on_it() {
    let mut transcript = Transcript::new();
    transcript.write(&format!("{DIM}done\n"));

    assert!(transcript.at_line_start());
}

#[test]
fn drops_the_oldest_lines_in_blocks_rather_than_one_at_a_time() {
    // One at a time would shift every row on every write, and a shifted row
    // zero costs a full redraw — which is the expensive path, once per line.
    let mut transcript = Transcript::new();
    for at in 0..12_001 {
        transcript.write(&format!("line {at}\n"));
    }

    let lines = transcript.lines();
    assert!(lines.len() < 12_001);
    assert_eq!(lines[lines.len() - 2], "line 12000");
}
