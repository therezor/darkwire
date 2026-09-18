//! The escape-sequence fixtures: what a first frame, a patch, a resize and a stop emit.

mod common;

use common::FakeOutput;
use darkwire_tui::{CLEAR_SCREEN, CURSOR_MARKER, Component, Renderer, RendererOptions};

const ESC: &str = "\x1b";

/// Every `\x1b[<n>A` (or `B`) in the output, as the numbers.
fn moves(text: &str, letter: char) -> Vec<usize> {
    text.split("\x1b[")
        .skip(1)
        .filter_map(|piece| {
            let digits: String = piece.chars().take_while(char::is_ascii_digit).collect();
            (piece[digits.len()..].starts_with(letter) && !digits.is_empty())
                .then(|| digits.parse().ok())
                .flatten()
        })
        .collect()
}

fn ups(text: &str) -> Vec<usize> {
    moves(text, 'A')
}

fn downs(text: &str) -> Vec<usize> {
    moves(text, 'B')
}

fn rows(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_owned()).collect()
}

fn renderer(columns: u16, screen_rows: u16) -> Renderer<FakeOutput> {
    Renderer::new(
        FakeOutput::new(columns, screen_rows),
        RendererOptions::default(),
    )
}

/// A component whose rows depend on the width, like a real one.
struct Ruled {
    head: &'static str,
    foot: &'static str,
}

impl Component for Ruled {
    fn render(&mut self, width: usize) -> Vec<String> {
        vec![
            self.head.to_owned(),
            "-".repeat(width),
            self.foot.to_owned(),
        ]
    }
}

#[test]
fn the_first_frame_prints_its_rows_and_erases_nothing() {
    let mut renderer = renderer(20, 10);
    renderer.render(&mut rows(&["one", "two", "three"]));

    let text = renderer.output().text();
    assert!(text.contains("one\r\ntwo\r\nthree"));
    // Nothing is erased on the way in, unless the caller asked for it: a
    // renderer drawing four rows of a picker under a shell prompt has no
    // business erasing what the shell printed.
    assert!(!text.contains(&format!("{ESC}[2J")));
    // Synchronized output wraps the frame by default.
    assert!(text.starts_with(&format!("{ESC}[?2026h")));
    assert!(text.ends_with(&format!("{ESC}[?2026l")));
    assert_eq!(renderer.full_redraws(), 1);
    assert_eq!(renderer.viewport_top(), 0);
    assert_eq!((renderer.columns(), renderer.rows()), (20, 10));
}

#[test]
fn the_first_frame_takes_the_screen_when_asked_and_not_the_history() {
    let options = RendererOptions {
        clear_on_first_frame: true,
        ..RendererOptions::default()
    };
    let mut renderer = Renderer::new(FakeOutput::new(20, 10), options);
    renderer.render(&mut rows(&["one", "two", "three"]));

    let text = renderer.output().text();
    assert!(text.contains(CLEAR_SCREEN));
    // `3J` erases the scrollback *buffer*, and nothing here ever sends it: what
    // is up there is the conversation, and the operator's own shell history
    // before it.
    assert!(!text.contains(&format!("{ESC}[3J")));
}

#[test]
fn clears_once_not_on_every_frame_after_it() {
    let options = RendererOptions {
        clear_on_first_frame: true,
        ..RendererOptions::default()
    };
    let mut renderer = Renderer::new(FakeOutput::new(20, 10), options);
    let mut view = rows(&["one", "two", "three"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    view[2] = "THREE".to_owned();
    renderer.render(&mut view);

    let text = renderer.output().text();
    assert!(!text.contains(&format!("{ESC}[2J")));
    assert!(text.contains("THREE"));
    assert_eq!(renderer.full_redraws(), 1);
}

#[test]
fn puts_the_terminal_cursor_where_the_marker_was() {
    let mut renderer = renderer(20, 10);
    renderer.render(&mut rows(&[
        "head",
        &format!("› ab{CURSOR_MARKER}"),
        "foot",
    ]));

    let text = renderer.output().text();
    // Up from the last row it wrote to the editor row, then along to the caret.
    assert_eq!(ups(text), [1]);
    assert!(text.contains(&format!("\r{ESC}[4C")));
    assert!(!text.contains(CURSOR_MARKER));
}

#[test]
fn the_first_marker_wins_and_every_marker_is_removed() {
    let mut renderer = renderer(20, 10);
    renderer.render(&mut rows(&[
        &format!("a{CURSOR_MARKER}b{CURSOR_MARKER}"),
        &format!("c{CURSOR_MARKER}"),
    ]));

    let text = renderer.output().text();
    assert!(!text.contains(CURSOR_MARKER));
    assert!(text.contains("ab\r\nc"));
    assert_eq!(ups(text), [1]);
    assert!(text.ends_with(&format!("\r{ESC}[1C{ESC}[?2026l")));
}

#[test]
fn can_run_without_synchronized_output() {
    let options = RendererOptions {
        synchronized: false,
        ..RendererOptions::default()
    };
    let mut renderer = Renderer::new(FakeOutput::new(20, 10), options);
    renderer.render(&mut rows(&["one"]));
    assert_eq!(renderer.output().text(), "one\r");
}

#[test]
fn falls_back_to_the_configured_size_when_the_device_reports_none() {
    let options = RendererOptions {
        columns: Some(30),
        rows: Some(5),
        ..RendererOptions::default()
    };
    let renderer = Renderer::new(FakeOutput::new(0, 0), options);
    assert_eq!((renderer.columns(), renderer.rows()), (30, 5));
}

#[test]
fn a_changed_frame_rewrites_only_the_rows_that_differ() {
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["one", "two", "three"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    view[2] = "CHANGED".to_owned();
    renderer.render(&mut view);

    let text = renderer.output().text();
    assert!(text.contains("CHANGED"));
    // The rows above it are not touched, which is what makes a keystroke cost
    // the editor row rather than the length of the conversation.
    assert!(!text.contains("one"));
    assert!(!text.contains("two"));
    assert_eq!(renderer.full_redraws(), 1);
}

#[test]
fn stops_at_the_last_row_that_differs_not_the_bottom_of_the_frame() {
    // A spinner changes one row ten times a second. Running to the bottom would
    // repaint the rules and the status bar with it — six rows of traffic for a
    // one-row change, which is what flicker is made of.
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["spin", "rule", "editor", "status"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    view[0] = "SPUN".to_owned();
    renderer.render(&mut view);

    let text = renderer.output().text();
    assert!(text.contains("SPUN"));
    assert!(!text.contains("rule"));
    assert!(!text.contains("status"));
}

#[test]
fn erases_the_rows_a_shorter_frame_no_longer_has() {
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["one", "two", "three"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    view.truncate(1);
    renderer.render(&mut view);

    assert!(renderer.output().text().contains(&format!("{ESC}[0J")));
}

#[test]
fn erases_everything_when_the_frame_empties() {
    // Only deletions and no row left to write: the walk still has to land on
    // a row before erasing below it.
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["one", "two"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    view.clear();
    renderer.render(&mut view);

    let text = renderer.output().text();
    assert!(text.contains(&format!("{ESC}[0J")));
    assert!(!text.contains("one"));
}

#[test]
fn makes_the_rows_a_grown_frame_needs_instead_of_moving_onto_ones_that_exist() {
    // The case every turn hits: the transcript gains rows while the frame is
    // already filling the screen. `CUD` past the bottom row does nothing at
    // all — only a newline scrolls — so reaching for the new rows that way
    // writes them over the last old one.
    let mut renderer = renderer(20, 5);
    let mut view = rows(&["one", "two", "three", "four", "five"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    view.extend(rows(&["six", "seven"]));
    renderer.render(&mut view);

    let text = renderer.output().text();
    assert!(text.contains("six"));
    assert!(text.contains("seven"));
    assert!(!text.contains(&format!("{ESC}[1B")));
    // And the last old row is still the last old row, not overwritten by the
    // first new one.
    assert!(!text.contains("five"));
    // Two rows scrolled off the top of a five-row window.
    assert_eq!(renderer.viewport_top(), 2);
}

#[test]
fn reprints_the_frame_when_a_row_in_the_scrollback_changes() {
    // A row that has scrolled off the top cannot be moved to, so the only
    // honest answer is the whole frame again — history and all.
    let mut renderer = renderer(20, 3);
    let mut view = rows(&["one", "two", "three", "four", "five"]);
    renderer.render(&mut view);
    assert_eq!(renderer.viewport_top(), 2);

    renderer.output_mut().reset();
    view[0] = "ONE".to_owned();
    renderer.render(&mut view);

    assert_eq!(renderer.full_redraws(), 2);
    assert!(renderer.output().text().contains(CLEAR_SCREEN));
}

#[test]
fn writes_nothing_but_a_cursor_move_when_nothing_changed() {
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["one", "two"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    renderer.render(&mut view);

    let text = renderer.output().text();
    assert!(!text.contains("one"));
    assert!(!text.contains("two"));
}

#[test]
fn a_resize_prints_the_whole_frame_again_rather_than_patching_it() {
    // The bug this whole design exists for. A terminal rewraps its own screen
    // before the process is told anything, so the rows a program drew are no
    // longer where it left them — some of them above the cursor, where no
    // erase can reach. Patching is not available; reprinting is.
    let mut renderer = renderer(40, 10);
    let mut view = Ruled {
        head: "head",
        foot: "foot",
    };
    renderer.render(&mut view);
    let before = renderer.full_redraws();

    renderer.output_mut().reset();
    renderer.output_mut().resize_to(20, 10);
    renderer.render(&mut view);

    let text = renderer.output().text();
    assert_eq!(renderer.full_redraws(), before + 1);
    assert!(text.contains(CLEAR_SCREEN));
    assert!(text.contains("head"));
    assert!(text.contains("foot"));
    assert!(text.contains(&"-".repeat(20)));
}

#[test]
fn a_resize_drops_the_scrollback_because_the_reflow_may_have_put_a_fragment_there() {
    // A narrowing can push the top of the old frame out of the viewport, and
    // erasing only what is on screen leaves that fragment above the new frame
    // — one copy per resize. Nothing the program can ask says whether it
    // happened, so the erase has to assume it did.
    let mut renderer = renderer(40, 24);
    let mut view = rows(&["head", "foot"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    renderer.output_mut().resize_to(20, 24);
    renderer.render(&mut view);

    assert!(renderer.output().text().contains(CLEAR_SCREEN));
}

#[test]
fn a_height_change_alone_reprints_too() {
    let mut renderer = renderer(40, 24);
    let mut view = rows(&["head", "foot"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    renderer.output_mut().resize_to(40, 12);
    renderer.render(&mut view);

    assert_eq!(renderer.full_redraws(), 2);
    assert!(renderer.output().text().contains(CLEAR_SCREEN));
}

#[test]
fn moves_by_the_same_amount_at_every_width_in_both_directions() {
    // The property the old footer could not hold. A frame whose height is a
    // function of the window width makes every cursor move a guess; asserting
    // the number would only pin today's arithmetic, so what is asserted is
    // that the number does not move.
    struct Chat;
    impl Component for Chat {
        fn render(&mut self, width: usize) -> Vec<String> {
            vec![
                "transcript".to_owned(),
                "-".repeat(width),
                format!("› typing{CURSOR_MARKER}"),
                "=".repeat(width),
            ]
        }
    }

    let mut renderer = renderer(120, 30);
    let mut view = Chat;
    renderer.render(&mut view);

    let mut seen = Vec::new();
    for width in [80, 120, 60, 120, 40] {
        renderer.output_mut().reset();
        renderer.output_mut().resize_to(width, 30);
        renderer.render(&mut view);
        seen.extend(ups(renderer.output().text()));
        seen.extend(downs(renderer.output().text()));
    }

    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), 1);
}

#[test]
fn cuts_a_row_wider_than_the_window_rather_than_letting_it_wrap() {
    // One entry is one row: a wrapped line would put every later row's
    // address out by one.
    let mut renderer = renderer(10, 10);
    renderer.render(&mut rows(&["abcdefghijklmnop", "x"]));

    let text = renderer.output().text();
    assert!(text.contains("abcdefghi…\r\nx"));
}

#[test]
fn coalesces_a_burst_of_requests_into_one_frame() {
    // A streaming turn asks for a render per token. Drawing each one would be
    // one frame per word arriving.
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["one"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    view[0] = "two".to_owned();
    assert!(!renderer.render_if_requested(&mut view));
    renderer.request_render();
    renderer.request_render();
    renderer.request_render();
    assert!(renderer.render_if_requested(&mut view));
    assert!(!renderer.render_if_requested(&mut view));

    assert_eq!(renderer.output().text().matches("two").count(), 1);
}

#[test]
fn sends_the_cursor_mode_only_when_it_actually_changes() {
    // A spinner repaints ten times a second, and a terminal taking DECTCEM on
    // every frame is doing ten times the work for no change.
    let mut renderer = renderer(20, 10);
    renderer.render(&mut rows(&["one"]));

    renderer.output_mut().reset();
    renderer.set_cursor_visible(true);
    assert_eq!(renderer.output().text(), "");

    renderer.set_cursor_visible(false);
    renderer.set_cursor_visible(false);
    assert_eq!(renderer.output().text(), format!("{ESC}[?25l"));
}

#[test]
fn stop_shows_the_cursor_and_leaves_it_below_the_frame() {
    let mut renderer = renderer(20, 10);
    renderer.render(&mut rows(&["one", "two"]));

    renderer.output_mut().reset();
    renderer.stop();

    let text = renderer.output().text();
    assert!(text.contains(&format!("{ESC}[?25h")));
    assert!(text.ends_with(&format!("\r\n{ESC}[?25h")));
    assert!(renderer.is_stopped());
}

#[test]
fn stop_moves_down_past_the_frame_when_the_cursor_was_parked_above_its_end() {
    let mut renderer = renderer(20, 10);
    renderer.render(&mut rows(&[
        &format!("› ab{CURSOR_MARKER}"),
        "rule",
        "status",
    ]));

    renderer.output_mut().reset();
    renderer.stop();

    assert_eq!(downs(renderer.output().text()), [2]);
}

#[test]
fn draws_nothing_once_stopped_however_often_it_is_asked() {
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["one"]);
    renderer.render(&mut view);
    // A request that landed before the stop is not a draw either.
    renderer.request_render();
    renderer.stop();

    renderer.output_mut().reset();
    renderer.stop();
    renderer.render(&mut view);
    assert!(!renderer.render_if_requested(&mut view));
    renderer.request_render();
    assert!(!renderer.render_if_requested(&mut view));
    renderer.set_cursor_visible(false);

    assert_eq!(renderer.output().text(), "");
}

#[test]
fn invalidate_prints_the_frame_whole_without_dropping_the_history() {
    // What a resize and Ctrl-L both ask for. The renderer cannot see that the
    // screen is wrong, because the rows it believes are up there are the rows it
    // wrote, so nothing short of being told will make it stop diffing.
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["one", "two", "three"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    renderer.invalidate();
    renderer.render(&mut view);

    let text = renderer.output().text();
    assert_eq!(renderer.full_redraws(), 2);
    assert!(text.contains(CLEAR_SCREEN));
    assert!(text.contains("one\r\ntwo\r\nthree"));
    // The scrollback is the conversation. A screen that needs repainting is no
    // reason to forget it.
    assert!(!text.contains(&format!("{ESC}[3J")));
}

#[test]
fn a_resize_keeps_the_scrollback() {
    let mut renderer = renderer(40, 24);
    let mut view = rows(&["head", "foot"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    renderer.output_mut().resize_to(20, 24);
    renderer.render(&mut view);

    assert!(!renderer.output().text().contains(&format!("{ESC}[3J")));
}

#[test]
fn render_if_requested_draws_once_however_often_it_was_asked() {
    // The whole point of the frame budget: a turn asks for a render per token
    // and gets one frame per tick.
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["one"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    view[0] = "ONE".to_owned();
    for _ in 0..50 {
        renderer.request_render();
    }
    assert!(renderer.render_if_requested(&mut view));
    assert!(!renderer.render_if_requested(&mut view));
    assert_eq!(renderer.output().text().matches("ONE").count(), 1);
}

#[test]
fn invalidate_is_honoured_through_the_requested_path_too() {
    // Both loops reach the renderer through `render_if_requested` while a turn
    // is streaming, so an invalidation that only `render` looked at would be
    // dropped for exactly as long as the answer lasted.
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["one", "two"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    renderer.invalidate();
    assert!(renderer.render_if_requested(&mut view));
    assert!(renderer.output().text().contains(CLEAR_SCREEN));
}

#[test]
fn print_above_writes_committed_lines_and_the_frame_in_one_bracket() {
    // Two brackets would erase the live region, show the gap, and paint it back
    // a blank flash per commit, which during a streamed answer is a flash per
    // batch of lines.
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["rule", "> editor"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    renderer.print_above(&rows(&["old one", "old two"]), &mut view);

    let text = renderer.output().text();
    assert_eq!(text.matches(&format!("{ESC}[?2026h")).count(), 1);
    assert_eq!(text.matches(&format!("{ESC}[?2026l")).count(), 1);
    assert!(text.contains("old one\r\nold two\r\n"));
    assert!(text.contains("rule\r\n> editor"));
}

#[test]
fn print_above_does_not_cut_what_it_commits_to_the_window() {
    // Everything else this renderer writes is cut to the width, because one
    // entry has to be one row for the arithmetic to hold. A committed line is
    // never addressed again, so the terminal folds it, and can refold it when
    // the window moves, which a line cut here never could.
    let mut renderer = renderer(10, 10);
    let mut view = rows(&["> editor"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    let long = "a line considerably wider than ten columns".to_owned();
    renderer.print_above(std::slice::from_ref(&long), &mut view);

    assert!(renderer.output().text().contains(&long));
}

#[test]
fn print_above_with_nothing_to_commit_is_an_ordinary_draw() {
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["one", "two"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    view[1] = "TWO".to_owned();
    renderer.print_above(&[], &mut view);

    let text = renderer.output().text();
    assert!(text.contains("TWO"));
    assert!(!text.contains("one"));
}

#[test]
fn a_frame_that_committed_is_diffed_normally_afterwards() {
    // The commit reprints the live region whole, and the next keystroke must go
    // back to touching one row. A renderer that forgot what it had just written
    // would repaint the region on every key for the rest of the session.
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["rule", "> ed"]);
    renderer.render(&mut view);
    renderer.print_above(&rows(&["committed"]), &mut view);

    renderer.output_mut().reset();
    view[1] = "> edi".to_owned();
    renderer.render(&mut view);

    let text = renderer.output().text();
    assert!(text.contains("> edi"));
    assert!(!text.contains("rule"));
}

#[test]
fn a_commit_still_honours_an_invalidation_that_was_waiting() {
    // A resize arriving mid-stream sets the invalidation; the very next frame
    // is as likely to be a commit as an ordinary draw. A commit that ignored it
    // would leave the terminal's reflow on screen until something else asked
    // for a redraw, which during a long answer is nothing at all.
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["rule", "> ed"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    renderer.invalidate();
    renderer.print_above(&rows(&["committed"]), &mut view);

    let text = renderer.output().text();
    assert!(text.contains(CLEAR_SCREEN));
    assert!(text.contains("committed"));
    assert!(text.contains("> ed"));
}

#[test]
fn an_ordinary_commit_does_not_clear_the_screen() {
    // The common path, and the reason the one above is worth distinguishing:
    // erasing the screen on every batch of a streamed answer would be a flash
    // per batch.
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["rule", "> ed"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    renderer.print_above(&rows(&["committed"]), &mut view);

    assert!(!renderer.output().text().contains(CLEAR_SCREEN));
}

#[test]
fn a_commit_never_writes_more_rows_than_the_window_has() {
    // What a resize does, and the shape of the bug it had. Printing past the
    // bottom scrolls, and what scrolls off is in the terminal's history where
    // the next erase cannot reach it. So every resize stranded another copy of
    // the screen above the new one. The caller sizes what it commits; this is
    // the assertion that says why it must.
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["one", "two", "rule", "> ed"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    renderer.invalidate();
    // Six committed rows under a four-row frame is exactly the ten the window
    // has, and one more would scroll.
    let tail = rows(&["a", "b", "c", "d", "e", "f"]);
    renderer.print_above(&tail, &mut view);

    let text = renderer.output().text();
    let printed = text.matches("\r\n").count() + 1;
    assert!(printed <= 10, "{printed} rows into a ten-row window");
}
