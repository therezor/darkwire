//! The escape-sequence fixtures: what a first frame, a patch, a resize and a stop emit.

mod common;

use common::FakeOutput;
use darkwire_tui::{CURSOR_MARKER, Component, Renderer, RendererOptions};

const ESC: &str = "\x1b";

/// `\x1b[0J`, which erases from the cursor down and never touches the history.
const ERASE_BELOW: &str = "\x1b[0J";

/// Whether `text` repainted by erasing the strip rather than the screen.
///
/// The distinction this whole file now turns on. `\x1b[2J` moves the erased rows
/// into the scrollback on most emulators, so a repaint left a copy of the
/// conversation in the history every time the window moved. Erasing downward
/// from a row the renderer owns cannot.
fn erases_the_strip(text: impl AsRef<str>) -> bool {
    let text = text.as_ref();
    text.contains(ERASE_BELOW) && !clears_the_screen(text)
}

/// Whether `text` erases the whole screen, in either of its two spellings.
fn clears_the_screen(text: impl AsRef<str>) -> bool {
    let text = text.as_ref();
    text.contains(&format!("{ESC}[2J")) || text.contains(&format!("{ESC}[3J"))
}

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
fn the_first_frame_scrolls_the_screen_away_and_erases_nothing() {
    // A screenful of newlines, so the strip lands on the last row and whatever
    // the shell had printed goes into the history. An erase would either lose
    // it or, on most emulators, copy it into the history twice over.
    let options = RendererOptions {
        take_screen_on_open: true,
        ..RendererOptions::default()
    };
    let mut renderer = Renderer::new(FakeOutput::new(20, 10), options);
    renderer.render(&mut rows(&["one", "two", "three"]));

    let text = renderer.output().text();
    assert!(text.contains(&"\n".repeat(10)));
    assert!(!clears_the_screen(text));
    assert!(!text.contains(ERASE_BELOW));
}

#[test]
fn the_strip_ends_on_the_last_row_it_was_given() {
    // The pin. Nothing is padded and nothing is measured against the window:
    // the screen was scrolled away once, the cursor is on the last row, and
    // the conversation only ever grows downward from there.
    let options = RendererOptions {
        take_screen_on_open: true,
        ..RendererOptions::default()
    };
    let mut renderer = Renderer::new(FakeOutput::new(20, 8), options);
    let mut view = rows(&["rule", "> ed", "status"]);
    renderer.render(&mut view);

    let text = renderer.output().text();
    let taken = text.find(&"\n".repeat(8)).expect("the screen is taken");
    let drawn = &text[taken + 8..];
    assert!(drawn.contains("rule\r\n> ed\r\nstatus"));
}

#[test]
fn clears_once_not_on_every_frame_after_it() {
    let options = RendererOptions {
        take_screen_on_open: true,
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
    assert!(text.ends_with(&format!("\r{ESC}[1C{ESC}[?7h{ESC}[?2026l")));
}

#[test]
fn can_run_without_synchronized_output() {
    let options = RendererOptions {
        synchronized: false,
        ..RendererOptions::default()
    };
    let mut renderer = Renderer::new(FakeOutput::new(20, 10), options);
    renderer.render(&mut rows(&["one"]));
    // Autowrap still brackets the rows: the synchronized bracket is about
    // tearing, and this one is about a row staying a row.
    assert_eq!(renderer.output().text(), format!("{ESC}[?7lone\r{ESC}[?7h"));
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
    assert!(erases_the_strip(renderer.output().text()));
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
    assert!(erases_the_strip(text));
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

    assert!(erases_the_strip(renderer.output().text()));
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
    assert!(erases_the_strip(renderer.output().text()));
}

#[test]
fn parks_the_cursor_the_same_distance_up_at_every_width() {
    // The property the old footer could not hold. A frame whose height is a
    // function of the window width makes every cursor move a guess; asserting
    // the number would only pin today's arithmetic, so what is asserted is
    // that the number does not move.
    //
    // Only the *last* move, which is the one that parks the cursor on the
    // editor row. The move before it is the repaint reaching the top of the
    // strip, and that one is meant to vary: it is how many rows the strip takes
    // at the new width, which is the whole of the resize arithmetic.
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
        let text = renderer.output().text();
        assert!(downs(text).is_empty(), "the editor row is above the last");
        seen.push(*ups(text).last().expect("a paint parks the cursor"));
    }

    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen, [1]);
}

#[test]
fn a_shorter_strip_gives_its_rows_back_at_the_top() {
    // Closing the command list used to lift the composer off the last row: the
    // strip was drawn from the same first row and simply got shorter, leaving
    // blank rows underneath it. The rows have to come off the top instead, and
    // what leaves the top is conversation the terminal already has.
    let mut renderer = renderer(20, 10);
    let mut tall = rows(&["one", "two", "three", "rule", "> ed"]);
    renderer.render(&mut tall);

    renderer.output_mut().reset();
    let mut short = rows(&["rule", "> ed"]);
    renderer.render(&mut short);

    let text = renderer.output().text();
    // Three rows fewer, so the screen scrolls three before the strip is drawn.
    assert!(text.matches("\r\n").count() >= 3, "{text:?}");
    assert!(text.contains("rule"));
    assert!(text.contains("> ed"));
}

#[test]
fn a_width_change_counts_nothing_and_takes_the_bottom_again() {
    // A terminal rewraps its own screen before the process hears about the
    // resize, and whether it rewraps a hard terminated row differs between
    // emulators. Counting rows to walk up by is a guess, and a guess that is
    // too large erases a band of the conversation and leaves blank rows where
    // it was. So the width change path counts nothing.
    let mut renderer = renderer(40, 10);
    let mut view = rows(&["a".repeat(38).as_str(), "rule", "> ed"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    renderer.output_mut().resize_to(20, 10);
    renderer.render(&mut view);

    let text = renderer.output().text();
    assert!(!clears_the_screen(text));
    assert!(text.contains(ERASE_BELOW));
    // No walk up before the erase: the first thing it does is erase downward.
    let erase = text.find(ERASE_BELOW).expect("it erases downward");
    let before = &text[..erase];
    assert!(
        !before.contains(&format!("{ESC}[")) || !before.contains('A'),
        "{before:?}"
    );
}

#[test]
fn draws_the_strip_with_autowrap_off_and_leaves_it_on() {
    // One entry is one row. Cutting each row to the window nearly gets there,
    // and a row exactly as wide as the window leaves some emulators in a
    // pending-wrap state that costs a row anyway. With autowrap off there is no
    // such state. It goes back on at the end of every paint, because the
    // conversation printed above the strip is the terminal's to fold.
    let mut renderer = renderer(4, 10);
    renderer.render(&mut rows(&["abcd", "x"]));

    let text = renderer.output().text();
    let off = text
        .find(&format!("{ESC}[?7l"))
        .expect("autowrap is turned off");
    let on = text
        .rfind(&format!("{ESC}[?7h"))
        .expect("autowrap is put back");
    assert!(off < text.find("abcd").expect("the row is drawn"));
    assert!(on > off);
}

#[test]
fn a_commit_wraps_the_conversation_and_not_the_strip() {
    // The two halves need opposite answers. A committed line is the terminal's
    // from then on, so it has to wrap and reflow; a strip row is addressed by
    // this renderer, so it must not.
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["rule", "> ed"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    renderer.print_above(&rows(&["a committed line"]), &mut view);

    let text = renderer.output().text();
    let committed = text.find("a committed line").expect("the line is printed");
    let off = text
        .find(&format!("{ESC}[?7l"))
        .expect("autowrap is turned off");
    assert!(
        committed < off,
        "the conversation goes out before autowrap is off"
    );
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
    assert!(erases_the_strip(text));
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
    assert!(erases_the_strip(renderer.output().text()));
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
    assert!(erases_the_strip(text));
    assert!(text.contains("committed"));
    assert!(text.contains("> ed"));
}

#[test]
fn an_ordinary_commit_erases_only_the_strip() {
    // The common path. Erasing the screen on every batch of a streamed answer
    // would be a flash per batch, and on most emulators a copy of the
    // conversation in the history per batch as well.
    let mut renderer = renderer(20, 10);
    let mut view = rows(&["rule", "> ed"]);
    renderer.render(&mut view);

    renderer.output_mut().reset();
    renderer.print_above(&rows(&["committed"]), &mut view);

    assert!(erases_the_strip(renderer.output().text()));
}

#[test]
fn nothing_it_ever_writes_clears_the_screen() {
    // The regression guard for the welcome banner appearing again and again in
    // the scrollback. Most emulators, including every one shipped on macOS,
    // move the rows `\x1b[2J` erases into the history rather than dropping them,
    // so a repaint duplicated whatever was on screen. There is no path here
    // that may emit it: not a first frame, not a resize, not an invalidation,
    // not a commit.
    let options = RendererOptions {
        take_screen_on_open: true,
        ..RendererOptions::default()
    };
    let mut renderer = Renderer::new(FakeOutput::new(20, 6), options);
    let mut view = rows(&["one", "two", "three"]);
    renderer.render(&mut view);
    renderer.invalidate();
    renderer.render(&mut view);
    renderer.output_mut().resize_to(12, 6);
    renderer.render(&mut view);
    renderer.output_mut().resize_to(12, 4);
    renderer.render(&mut view);
    renderer.print_above(&rows(&["committed"]), &mut view);

    assert!(!clears_the_screen(renderer.output().text()));
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
