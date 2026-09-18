//! A block folds or it does not, and the fold is what a collapsed one shows.

use darkwire_tui::Block;

const DIM: &str = "\x1b[2m";

#[test]
fn prose_has_no_summary_and_cannot_fold() {
    // The distinction the type exists to make. An answer with a `▸` in front of
    // it that hides when pressed is not an answer, it is a disclosure. The only
    // honest way to refuse that is to have nothing to show in its place.
    let mut block = Block::prose();
    block.write("the answer\n");
    assert!(!block.folds());

    block.set_collapsed(true);
    assert!(!block.collapsed());
    assert_eq!(block.render(40), ["the answer"]);
}

#[test]
fn a_folded_block_shows_its_summary_and_nothing_else() {
    let mut block = Block::folding("reasoning", "thinking… 4s", true);
    block.write("first thought\nsecond thought\n");
    assert_eq!(block.render(40), ["thinking… 4s"]);
}

#[test]
fn an_unfolded_block_shows_the_summary_above_the_body() {
    // Both, not one: the summary is what says whose text this is, and losing it
    // on expand would leave a run of reasoning that reads as the answer.
    let mut block = Block::folding("reasoning", "thought for 4s", false);
    block.write("first thought\nsecond thought");
    assert_eq!(
        block.render(40),
        ["thought for 4s", "first thought", "second thought"],
    );
}

#[test]
fn the_summary_can_be_replaced_while_the_block_runs() {
    // Which is the whole reason a collapsed block is not simply hidden: a fold
    // that never changes for the length of a long reasoning run is
    // indistinguishable from a terminal that has stopped.
    let mut block = Block::folding("reasoning", "thinking… 1s", true);
    block.set_summary("thinking… 12s");
    assert_eq!(block.render(40), ["thinking… 12s"]);
}

#[test]
fn prose_ignores_a_summary_it_was_never_given() {
    let mut block = Block::prose();
    block.set_summary("this is not shown anywhere");
    block.write("the answer\n");
    assert_eq!(block.render(40), ["the answer"]);
}

#[test]
fn a_summary_too_wide_for_the_window_folds_rather_than_being_cut_here() {
    // The renderer cuts a row to the window; this wraps it. Doing both would
    // mean the summary of a tool call with a long argument vanished at narrow
    // widths, and the row it was cut to is the renderer's decision to make.
    let mut block = Block::folding("tool", "exec · a command with a long argument", true);
    let rows = block.render(20);
    assert!(rows.len() > 1);
    for row in &rows {
        assert!(row.chars().count() <= 20, "row too wide: {row:?}");
    }
    for word in ["exec", "command", "argument"] {
        assert!(rows.iter().any(|row| row.contains(word)), "lost {word}");
    }
}

#[test]
fn a_style_left_open_does_not_escape_the_block_that_opened_it() {
    // Blocks are written to one after another, and a dim reasoning run that
    // leaked its `\x1b[2m` would dim the answer under it. The state that
    // carries a style across a line break belongs to the block for this reason.
    let mut reasoning = Block::folding("reasoning", "thought", false);
    reasoning.write(&format!("{DIM}dimmed\nstill dimmed"));

    let mut answer = Block::prose();
    answer.write("plain\n");
    assert_eq!(answer.render(40), ["plain"]);
}

#[test]
fn a_style_spanning_a_break_reopens_on_the_next_line_within_a_block() {
    let mut block = Block::prose();
    block.write(&format!("{DIM}one\ntwo"));
    let rows = block.render(40);
    assert!(rows[1].starts_with(DIM), "got {:?}", rows[1]);
}

#[test]
fn reports_whether_the_last_write_ended_a_line() {
    let mut block = Block::prose();
    assert!(block.at_line_start());
    block.write("half");
    assert!(!block.at_line_start());
    block.write(" a line\n");
    assert!(block.at_line_start());
}

#[test]
fn draining_the_front_leaves_the_rest_where_it_was() {
    let mut block = Block::prose();
    block.write("one\ntwo\nthree\nfour");
    block.render(40);
    block.drain_front(2);
    assert_eq!(block.render(40), ["three", "four"]);
}

#[test]
fn the_line_a_write_left_open_is_not_a_row() {
    // It holds the styles still open and nothing else, and it is where the next
    // chunk lands. Drawing it put a blank row under every message, which
    // whatever sits below the transcript then had to reason about.
    let mut block = Block::prose();
    block.write("the answer\n");
    assert_eq!(block.render(40), ["the answer"]);

    // Still there, still taking the next write.
    block.write("half");
    assert_eq!(block.render(40), ["the answer", "half"]);
}

#[test]
fn a_blank_line_inside_the_text_is_still_a_row() {
    // The paragraph break a model asked for. Only the line left open at the
    // bottom is not text; a blank between two lines is.
    let mut block = Block::prose();
    block.write("one\n\ntwo\n");
    assert_eq!(block.render(40), ["one", "", "two"]);
}

#[test]
fn the_height_is_the_number_of_rows_it_draws() {
    // The renderer addresses one row per entry and the transcript's cap is
    // measured in rows, so a height that disagreed with the drawing would
    // commit a line of conversation that had fitted.
    let mut mid_line = Block::prose();
    mid_line.write("half");

    let mut open_line = Block::prose();
    open_line.write("done\n");

    let mut styled_open = Block::prose();
    styled_open.write(&format!("{DIM}done\n"));

    let empty = Block::prose();

    for mut block in [mid_line, open_line, styled_open, empty] {
        assert_eq!(block.height(40), block.render(40).len());
    }
}

#[test]
fn a_collapsed_block_still_shows_its_summary() {
    // The open line is dropped from the body, never from the row standing in
    // for it.
    let mut block = Block::folding("reasoning", "thought for 4s", true);
    block.write("a thought\n");
    assert_eq!(block.render(40), ["thought for 4s"]);
    assert_eq!(block.height(40), 1);
}
