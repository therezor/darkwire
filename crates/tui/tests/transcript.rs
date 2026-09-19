//! Fragments become logical lines, refolded per width, styles carried across breaks, bounded.

use darkwire_tui::{Component, Transcript};
use proptest::prelude::*;

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

    assert_eq!(transcript.render(40), ["one two three four five six"]);
    assert_eq!(transcript.render(12), ["one two", "three four", "five six"]);
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
    assert_eq!(transcript.render(40), ["first", "second"]);
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

// ------------------------------------------------------------------- blocks

#[test]
fn prose_with_no_blocks_reads_exactly_as_it_used_to() {
    // The degenerate case, and the one that has to stay free: a conversation
    // that never opens a run is one block, and nothing about it costs more than
    // a list of lines did.
    let mut transcript = Transcript::new();
    transcript.write("the answer\ncontinues\n");
    assert_eq!(transcript.render(40), ["the answer", "continues"]);
}

#[test]
fn a_run_opened_and_closed_keeps_what_came_after_it_out() {
    // The whole reason blocks exist. A flat buffer cannot say where the
    // reasoning stopped, so a fold drawn over it eats the first line of the
    // answer.
    let mut transcript = Transcript::new();
    transcript.write("before\n");
    transcript.open_block("reasoning", "thought", false);
    transcript.write("thinking\n");
    transcript.close_block();
    transcript.write("after\n");

    // No blank row between `before` and the fold. The line the last write left
    // open is where the next thing goes, and the next thing is the block.
    transcript.set_collapsed("reasoning", true);
    assert_eq!(transcript.render(40), ["before", "thought", "after"]);
}

#[test]
fn folding_reaches_every_run_of_that_kind_not_just_the_last() {
    // The key is pressed to answer "show me what it was thinking", and a turn
    // that called three tools has three runs of it.
    let mut transcript = Transcript::new();
    for at in 0..3 {
        transcript.open_block("reasoning", &format!("thought {at}"), false);
        transcript.write(&format!("body {at}\n"));
        transcript.close_block();
    }

    transcript.set_collapsed("reasoning", true);
    assert_eq!(
        transcript.render(40),
        ["thought 0", "thought 1", "thought 2"],
    );

    transcript.set_collapsed("reasoning", false);
    assert!(transcript.render(40).iter().any(|row| row == "body 1"));
}

#[test]
fn one_tag_folds_without_touching_another() {
    let mut transcript = Transcript::new();
    transcript.open_block("reasoning", "thought", false);
    transcript.write("thinking\n");
    transcript.close_block();
    transcript.open_block("tool", "exec", false);
    transcript.write("output\n");
    transcript.close_block();

    transcript.set_collapsed("reasoning", true);
    let rows = transcript.render(40);
    assert!(!rows.iter().any(|row| row == "thinking"));
    assert!(rows.iter().any(|row| row == "output"));
}

#[test]
fn the_open_run_is_the_one_whose_summary_moves() {
    let mut transcript = Transcript::new();
    transcript.open_block("reasoning", "thinking… 1s", true);
    transcript.write("a thought\n");
    transcript.set_summary("thinking… 9s");
    assert_eq!(transcript.render(40), ["thinking… 9s"]);
}

#[test]
fn opening_a_run_straight_after_closing_one_leaves_no_gap() {
    // A block nobody wrote to draws nothing and still costs a comparison on
    // every frame, which at thirty frames a second is worth not having.
    let mut transcript = Transcript::new();
    transcript.open_block("tool", "read", true);
    transcript.write("output\n");
    transcript.close_block();
    transcript.open_block("tool", "write", true);
    transcript.write("output\n");
    transcript.close_block();

    assert_eq!(transcript.render(40), ["read", "write"]);
}

#[test]
fn clearing_leaves_somewhere_to_write() {
    let mut transcript = Transcript::new();
    transcript.open_block("reasoning", "thought", true);
    transcript.write("thinking\n");
    transcript.clear();
    transcript.write("fresh\n");

    assert_eq!(transcript.render(40), ["fresh"]);
}

#[test]
fn the_bound_takes_a_whole_run_with_its_summary() {
    // A summary row for a body nobody can read any more is worse than nothing,
    // so a run small enough to fit in the batch goes entirely, the row that
    // labels it included.
    let mut transcript = Transcript::new();
    transcript.open_block("tool", "the first call", false);
    for at in 0..100 {
        transcript.write(&format!("old {at}\n"));
    }
    transcript.close_block();
    for at in 0..12_000 {
        transcript.write(&format!("new {at}\n"));
    }

    let rows = transcript.render(40);
    assert!(!rows.iter().any(|row| row == "the first call"));
    assert!(!rows.iter().any(|row| row == "old 0"));
    assert!(rows.iter().any(|row| row == "new 11999"));
}

#[test]
fn a_run_too_big_for_the_batch_is_trimmed_and_keeps_its_summary() {
    // The other half of the rule. Dropping 12,000 lines to stay under a bound
    // of 10,000 would throw away most of a session to reclaim a batch, so a run
    // larger than the batch gives up its oldest lines and stays.
    let mut transcript = Transcript::new();
    transcript.open_block("tool", "one enormous call", false);
    for at in 0..12_000 {
        transcript.write(&format!("old {at}\n"));
    }

    let rows = transcript.render(40);
    assert!(rows.iter().any(|row| row == "one enormous call"));
    assert!(!rows.iter().any(|row| row == "old 0"));
    assert!(rows.iter().any(|row| row == "old 11999"));
}

// ------------------------------------------------- the incremental wrap cache

/// The rows a transcript that had been written all at once would hand back.
///
/// The oracle for every case below: an incremental cache is only correct if it
/// cannot be told apart from a clean one, and the only way to say that without
/// restating the wrap algorithm is to build a clean one and compare.
fn from_scratch(writes: &[String], width: usize) -> Vec<String> {
    let mut fresh = Transcript::new();
    for text in writes {
        fresh.write(text);
    }
    fresh.render(width)
}

#[test]
fn a_write_with_no_newline_still_refolds_the_line_it_lengthened() {
    // The case a length comparison misses. `logical.len()` does not move, and
    // the rows do: this is every streamed token that is not the end of a line.
    let mut transcript = Transcript::new();
    transcript.write("word ".repeat(6).as_str());
    let before = transcript.render(20);
    transcript.write("and several more words after it");
    let after = transcript.render(20);

    assert_ne!(before, after);
    assert_eq!(
        after,
        from_scratch(
            &[
                "word ".repeat(6),
                "and several more words after it".to_owned(),
            ],
            20,
        ),
    );
}

#[test]
fn appending_matches_a_clean_render_at_every_step() {
    let writes: Vec<String> = [
        "one two three four five six seven\n",
        "eight",
        " nine ten\n\n",
        "eleven twelve thirteen fourteen fifteen sixteen seventeen\n",
        "eighteen",
    ]
    .iter()
    .map(|text| (*text).to_owned())
    .collect();

    let mut transcript = Transcript::new();
    for at in 0..writes.len() {
        transcript.write(&writes[at]);
        assert_eq!(
            transcript.render(24),
            from_scratch(&writes[..=at], 24),
            "after write {at}",
        );
    }
}

#[test]
fn a_width_change_rebuilds_rather_than_resuming() {
    // Nothing can be kept: a line that was one row at 80 is three at 24, so
    // every row after it has moved.
    let writes: Vec<String> = ["alpha beta gamma delta epsilon zeta eta theta\niota\n"]
        .iter()
        .map(|text| (*text).to_owned())
        .collect();

    let mut transcript = Transcript::new();
    transcript.write(&writes[0]);
    transcript.render(80);

    assert_eq!(transcript.render(24), from_scratch(&writes, 24));
    assert_eq!(transcript.render(80), from_scratch(&writes, 80));
}

#[test]
fn a_style_carried_across_a_break_survives_the_incremental_path() {
    // The rows this produces are not the text that was written, so a cache that
    // resumed from the wrong place would show it here first.
    let writes: Vec<String> = [format!("{DIM}thinking"), " about it\nplainly\n".to_owned()]
        .iter()
        .map(String::clone)
        .collect();

    let mut transcript = Transcript::new();
    transcript.write(&writes[0]);
    transcript.render(30);
    transcript.write(&writes[1]);

    assert_eq!(transcript.render(30), from_scratch(&writes, 30));
}

#[test]
fn clearing_and_writing_again_does_not_resume_from_what_went() {
    let mut transcript = Transcript::new();
    transcript.write("the first conversation, at some length\n");
    transcript.render(20);
    transcript.clear();
    transcript.write("the second\n");

    assert_eq!(
        transcript.render(20),
        from_scratch(&["the second\n".to_owned()], 20),
    );
}

proptest! {
    // The cache is the one place in this file where a correct answer and a
    // fast one are written differently, so the property worth asserting is that
    // they cannot be told apart. Generated writes, because the shapes that
    // break a resume point are the ones nobody thinks to type: a chunk that is
    // only a newline, a chunk that is empty, a line that is exactly the width.

    #[test]
    fn an_incrementally_built_cache_matches_a_clean_one(
        writes in prop::collection::vec("[ -~\n]{0,24}", 1..12),
        width in 4_usize..=40,
    ) {
        let mut transcript = Transcript::new();
        for text in &writes {
            transcript.write(text);
            // Rendered after every write, which is what forces the incremental
            // path. Rendering only at the end would rebuild once and prove
            // nothing.
            transcript.render(width);
        }
        prop_assert_eq!(transcript.render(width), from_scratch(&writes, width));
    }

    #[test]
    fn a_render_at_another_width_in_the_middle_changes_nothing(
        writes in prop::collection::vec("[ -~\n]{0,24}", 1..8),
        narrow in 4_usize..=20,
        wide in 21_usize..=60,
    ) {
        let mut transcript = Transcript::new();
        for text in &writes {
            transcript.write(text);
            transcript.render(narrow);
            transcript.render(wide);
        }
        prop_assert_eq!(transcript.render(narrow), from_scratch(&writes, narrow));
        prop_assert_eq!(transcript.render(wide), from_scratch(&writes, wide));
    }
}

// ------------------------------------------------------- the scrollback line

#[test]
fn a_frame_that_fits_commits_nothing() {
    // Every frame of a short session, and most frames of a long one. The
    // scrollback is where text goes when there is nowhere left to put it, not
    // a place text passes through.
    let mut transcript = Transcript::new();
    transcript.write("one\ntwo\n");
    assert!(transcript.take_committable(20, 40).is_empty());
    assert_eq!(transcript.render(40), ["one", "two"]);
}

#[test]
fn the_open_line_never_goes_even_when_the_frame_is_over() {
    // It is still being written to. Committed text cannot be rewritten, so a
    // half-streamed sentence in the history is a sentence that stays half.
    let mut transcript = Transcript::new();
    for at in 0..40 {
        transcript.write(&format!("line {at}\n"));
    }
    transcript.write("still arriv");
    transcript.take_committable(4, 40);

    let rows = transcript.render(40);
    assert!(rows.iter().any(|row| row == "still arriv"));
}

#[test]
fn a_turn_too_tall_for_the_window_is_kept_inside_its_cap() {
    // The exchange on screen is held so its folds stay reachable, and a turn
    // longer than the window cannot be. Past the cap the oldest of it goes,
    // which is the part the reader has finished with.
    let mut transcript = Transcript::new();
    for at in 0..200 {
        transcript.write(&format!("line {at}\n"));
        transcript.give_up_the_fold(10, 40);
        assert!(
            transcript.height(40) <= 10,
            "what is held grew to {} rows",
            transcript.height(40),
        );
    }
}

#[test]
fn what_is_on_screen_stays_until_the_next_exchange_starts() {
    // The whole point of the boundary. `ctrl-t` unfolds a run the reader can
    // see, so a run they can see has to still be here to unfold.
    let mut transcript = Transcript::new();
    transcript.open_block("reasoning", "thought for 4s", true);
    transcript.write("a private thought\n");
    transcript.close_block();
    transcript.write("the answer\n");

    assert!(transcript.take_committable(0, 40).is_empty());
    transcript.set_collapsed("reasoning", false);
    assert!(transcript.lines().contains(&"a private thought"));

    transcript.start_turn();
    assert!(!transcript.take_committable(0, 40).is_empty());
}

#[test]
fn what_is_committed_is_what_was_on_screen_in_order() {
    let mut transcript = Transcript::new();
    for at in 0..60 {
        transcript.write(&format!("line {at}\n"));
    }
    transcript.start_turn();
    let committed = transcript.take_committable(8, 40);

    // The history and the screen are one conversation cut in two, not two views
    // of it: put them back together and nothing has moved, repeated or gone.
    let mut rejoined = committed.clone();
    rejoined.extend(transcript.render(40));

    let mut whole = Transcript::new();
    for at in 0..60 {
        whole.write(&format!("line {at}\n"));
    }
    assert_eq!(rejoined, whole.render(40));
    assert!(!committed.is_empty());
}

#[test]
fn a_folded_run_commits_the_row_that_was_showing_and_not_the_body() {
    // The history is what was on screen. Committing the body of something the
    // reader had folded away would put it in the scrollback precisely because
    // they asked not to see it.
    let mut transcript = Transcript::new();
    transcript.open_block("reasoning", "thought for 4s", true);
    for at in 0..40 {
        transcript.write(&format!("secret {at}\n"));
    }
    transcript.close_block();
    transcript.write("the answer\n");
    // The exchange is over, so it may go. Until it is, the fold stays on screen
    // where the keys can still reach it.
    transcript.start_turn();

    let committed = transcript.take_committable(1, 40);
    assert!(committed.iter().any(|line| line == "thought for 4s"));
    assert!(!committed.iter().any(|line| line.starts_with("secret")));
}

#[test]
fn an_open_run_holds_the_line_rather_than_freezing_a_summary_that_moves() {
    // Its summary counts up. Half of it in the history would leave whichever
    // figure it happened to be showing there for good.
    let mut transcript = Transcript::new();
    transcript.open_block("reasoning", "thinking… 1s", false);
    for at in 0..40 {
        transcript.write(&format!("thought {at}\n"));
    }

    assert!(transcript.take_committable(4, 40).is_empty());
    transcript.close_block();
    transcript.write("done\n");
    // Still nothing: the exchange is on screen until the next one starts.
    assert!(transcript.take_committable(4, 40).is_empty());
    transcript.start_turn();
    assert!(!transcript.take_committable(4, 40).is_empty());
}

#[test]
fn a_run_that_said_nothing_leaves_no_summary_behind() {
    // A provider opening and closing its reasoning channel with nothing in it
    // is ordinary. A fold that opens onto an empty body is not.
    let mut transcript = Transcript::new();
    transcript.write("before\n");
    transcript.open_block("reasoning", "thought for 0s", false);
    transcript.close_block();
    transcript.write("after\n");

    let rows = transcript.render(40);
    assert!(!rows.iter().any(|row| row.contains("thought for")));
    assert!(rows.iter().any(|row| row == "before"));
    assert!(rows.iter().any(|row| row == "after"));
}

#[test]
fn an_expanded_run_puts_no_blank_row_between_its_summary_and_its_body() {
    // The summary arrives as a written line, newline and all, and a summary
    // that kept it would draw an empty row under itself on every fold.
    let mut transcript = Transcript::new();
    transcript.open_block("tool", "  ok 1.2s", false);
    transcript.write("    first line of output\n");
    transcript.close_block();

    let rows = transcript.render(40);
    assert_eq!(rows[0], "  ok 1.2s");
    assert_eq!(rows[1], "    first line of output");
}

#[test]
fn the_line_a_write_left_open_does_not_become_a_blank_row_above_a_fold() {
    // Every message is written with a trailing newline, so every fold that
    // follows one had an empty row over it. Once reasoning folds by default,
    // that is a blank line between what you typed and the answer to it.
    let mut transcript = Transcript::new();
    transcript.write("› how are you\n");
    transcript.open_block("reasoning", "┄ thought 400ms", true);
    transcript.write("some thinking\n");
    transcript.close_block();

    assert_eq!(transcript.render(40), ["› how are you", "┄ thought 400ms"]);
}

#[test]
fn a_write_after_a_run_starts_its_own_line() {
    // The line trimmed above is where the next write would have landed, so
    // dropping it has to leave somewhere else for the answer to go. Without
    // that, the first chunk after a run joins the end of the line before it:
    // `beforeafter`.
    let mut transcript = Transcript::new();
    transcript.write("before\n");
    transcript.open_block("reasoning", "thought", true);
    transcript.write("thinking\n");
    transcript.close_block();
    transcript.write("after\n");

    let rows = transcript.render(40);
    assert!(rows.iter().any(|row| row == "before"), "{rows:?}");
    assert!(rows.iter().any(|row| row == "after"), "{rows:?}");
}

#[test]
fn a_write_after_a_run_that_said_nothing_also_starts_its_own_line() {
    // The same, down the path where the empty run is dropped rather than kept.
    // That path put the previous block back on the end of the list, trimmed,
    // and the next write joined onto it.
    let mut transcript = Transcript::new();
    transcript.write("before\n");
    transcript.open_block("reasoning", "thought", true);
    transcript.close_block();
    transcript.write("after\n");

    let rows = transcript.render(40);
    assert!(rows.iter().any(|row| row == "before"), "{rows:?}");
    assert!(rows.iter().any(|row| row == "after"), "{rows:?}");
    assert!(
        !rows.iter().any(|row| row.contains("beforeafter")),
        "{rows:?}"
    );
}

#[test]
fn the_live_region_is_the_same_height_whether_or_not_the_last_write_ended_a_line() {
    // What the frame above the editor rests on. The predicate that used to
    // decide the gap could not tell a block holding an open line from one
    // holding nothing, and the two drew a different number of rows, so the gap
    // was two rows about as often as it was one.
    let mut mid_line = Transcript::new();
    mid_line.write("hi");

    let mut ended = Transcript::new();
    ended.write("hi\n");

    assert_eq!(mid_line.height(40), ended.height(40));
    assert_eq!(mid_line.render(40), ended.render(40));
}

#[test]
fn a_run_that_shows_nothing_does_not_pin_the_live_region() {
    // It is opened, written and closed in one step, so the block behind it is
    // prose again and the commit has something it may take. A run left open
    // would hold the live region for the rest of the session.
    let mut transcript = Transcript::new();
    transcript.write("the answer\n");
    transcript.hide_block("stats", true);
    transcript.write("  · 2 steps · 26ms\n");
    transcript.close_block();
    for at in 0..40 {
        transcript.write(&format!("line {at}\n"));
    }
    transcript.start_turn();

    let committed = transcript.take_committable(2, 40);
    assert!(!committed.is_empty());
    assert!(!committed.iter().any(|line| line.contains("2 steps")));
    assert!(!committed.iter().any(String::is_empty));
}

#[test]
fn the_key_reaches_a_run_that_shows_nothing() {
    // The whole reason it is a block rather than prose written conditionally:
    // what is already on screen has to follow the switch.
    let mut transcript = Transcript::new();
    transcript.hide_block("stats", true);
    transcript.write("  · 2 steps · 26ms\n");
    transcript.close_block();
    assert!(transcript.render(40).is_empty());

    transcript.set_collapsed("stats", false);
    assert_eq!(transcript.render(40), ["  · 2 steps · 26ms"]);
}

#[test]
fn says_whether_anything_has_gone_to_the_scrollback() {
    // What a frame asks to know whether the screen above it holds its own
    // conversation or is still whatever the shell left there.
    let mut transcript = Transcript::new();
    transcript.write("one\ntwo\n");
    assert!(!transcript.committed_anything());

    for at in 0..60 {
        transcript.write(&format!("line {at}\n"));
    }
    transcript.start_turn();
    assert!(!transcript.take_committable(4, 40).is_empty());
    assert!(transcript.committed_anything());

    transcript.clear();
    assert!(!transcript.committed_anything());
}

#[test]
fn a_window_that_shrank_forgets_the_rows_it_scrolled_away() {
    // The terminal moved them into its history to keep the cursor visible, so
    // they are already up there. Committing them would print a second copy
    // directly under the first, which is the duplicate this whole rewrite is
    // about.
    let mut transcript = Transcript::new();
    for at in 0..10 {
        transcript.write(&format!("line {at}\n"));
    }
    transcript.forget_front(3, 40);
    transcript.start_turn();

    let left = transcript.take_committable(0, 40);
    assert!(!left.iter().any(|line| line.contains("line 0")));
    assert!(!left.iter().any(|line| line.contains("line 2")));
    assert!(left.iter().any(|line| line.contains("line 3")));
}

#[test]
fn forgetting_counts_rows_rather_than_lines() {
    // A line that wraps to three rows accounts for three of the rows the window
    // lost. Counting it as one drops three times too much.
    let mut transcript = Transcript::new();
    for at in 0..6 {
        transcript.write(&format!(
            "line {at} with enough words on it to wrap at forty\n"
        ));
    }
    transcript.forget_front(3, 40);
    transcript.start_turn();

    let left = transcript.take_committable(0, 40);
    // Each of those wraps to two rows, so three rows takes two lines and a
    // row's worth of the third is forgotten with them. Lines go whole: half a
    // line in the history and half on screen is worse than a row that is only
    // in the history, which is where it already was.
    assert!(!left.iter().any(|line| line.starts_with("line 0")));
    assert!(!left.iter().any(|line| line.starts_with("line 1")));
    assert!(left.iter().any(|line| line.starts_with("line 2")));
}

#[test]
fn forgetting_never_takes_the_line_still_being_written() {
    let mut transcript = Transcript::new();
    transcript.write("half a sen");
    transcript.forget_front(20, 40);

    assert_eq!(transcript.lines(), ["half a sen"]);
}

#[test]
fn a_run_too_tall_to_hold_gives_up_its_fold() {
    // A tool printing more than the window was holding the screen on a promise
    // that the reader could still change its mind. Past the cap the promise is
    // what goes: the lines are printed, and what is printed is history.
    let mut transcript = Transcript::new();
    // Expanded, because a folded run is one row and never outgrows anything.
    transcript.open_block("tool", "ok 1.2s", false);
    for at in 0..40 {
        transcript.write(&format!("output {at}\n"));
    }

    // Nothing may go while the run is open, however tall it is.
    assert!(transcript.take_committable(0, 40).is_empty());

    let given = transcript.give_up_the_fold(4, 40);
    assert!(!given.is_empty());
    assert!(given.iter().any(|line| line.contains("output 0")));
    assert!(transcript.height(40) <= 4);
}

#[test]
fn giving_up_the_fold_keeps_the_line_still_open() {
    let mut transcript = Transcript::new();
    transcript.open_block("tool", "ok 1.2s", false);
    for at in 0..40 {
        transcript.write(&format!("output {at}\n"));
    }
    transcript.write("still writing");

    transcript.give_up_the_fold(1, 40);
    assert_eq!(transcript.lines().last(), Some(&"still writing"));
}
