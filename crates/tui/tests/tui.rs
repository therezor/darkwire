//! The live area growing, shrinking, moving, and letting history past.

use std::io::Write;
use std::time::Duration;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use darkwire_tui::insert_history::InsertMode;
use darkwire_tui::terminal::Terminal;
use darkwire_tui::testkit::VT100Backend;
use darkwire_tui::tui::{Tui, TuiEvent, map_event};
use darkwire_tui::{Key, KeyName};
use futures::StreamExt;
use ratatui::layout::{Position, Rect, Size};
use ratatui::text::Line;
use ratatui::widgets::Widget;

/// A live area `rows` tall at `top` of a `width` by `height` screen.
fn open(width: u16, height: u16, top: u16, rows: u16) -> Tui<VT100Backend> {
    let backend = VT100Backend::new(width, height);
    let mut terminal =
        Terminal::anchored(backend, Size { width, height }, Position { x: 0, y: top });
    terminal.set_viewport_area(Rect::new(0, top, width, rows));
    Tui::new(terminal, InsertMode::ScrollRegion)
}

/// Fills the live area with numbered rows so a test can see where it is.
#[allow(
    clippy::expect_used,
    reason = "a frame the backend refused is the failure the test is looking for"
)]
fn draw(tui: &mut Tui<VT100Backend>, height: u16, text: &str) {
    let text = text.to_owned();
    tui.draw(height, |frame| {
        let area = frame.area;
        for offset in 0..area.height {
            let line = Line::from(format!("{text}{offset}"));
            Widget::render(
                &line,
                Rect::new(area.x, area.y + offset, area.width, 1),
                frame.buffer,
            );
        }
    })
    .expect("draw");
}

fn plain(text: &str) -> Line<'static> {
    Line::from(text.to_owned())
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn the_live_area_sits_on_the_bottom_of_the_screen() {
    let mut tui = open(20, 8, 6, 2);
    draw(&mut tui, 2, "live");

    let rows = tui.terminal().backend().rows();
    assert_eq!(rows[6], "live0");
    assert_eq!(rows[7], "live1");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_live_area_that_grew_scrolls_the_history_above_it() {
    let mut tui = open(20, 8, 6, 2);
    draw(&mut tui, 2, "live");
    tui.insert_history_lines(vec![plain("older"), plain("newer")]);
    draw(&mut tui, 2, "live");
    assert!(
        tui.terminal()
            .backend()
            .rows()
            .iter()
            .any(|row| row == "older"),
        "the history is not on the screen"
    );

    // Three rows will not fit under row 6 on an eight-row screen.
    draw(&mut tui, 3, "live");

    let area = tui.terminal().viewport_area;
    assert_eq!(area.height, 3);
    assert_eq!(area.bottom(), 8, "the live area hangs off the screen");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_live_area_that_shrank_does_not_leave_its_old_rows_behind() {
    let mut tui = open(20, 8, 5, 3);
    draw(&mut tui, 3, "tall");
    assert_eq!(tui.terminal().backend().row(5), "tall0");

    draw(&mut tui, 1, "short");

    // The top held, so the one row it kept is where the first row was, and
    // everything it gave back below is blank.
    let rows = tui.terminal().backend().rows();
    assert_eq!(rows[5], "short0");
    assert!(
        !rows.iter().any(|row| row.starts_with("tall")),
        "a row of the taller live area survived: {rows:?}"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_live_area_that_shrank_and_wrote_history_leaves_no_gap() {
    // What a folded tool call does: the body grows the live area while it
    // streams, then one summary row is all that reaches the scrollback. The
    // rows the area gave back are above it now, so they are history, and
    // history nothing ever wrote into is a blank gap the reader has to scroll
    // past.
    let mut tui = open(20, 10, 8, 2);
    draw(&mut tui, 2, "live");
    tui.insert_history_lines(vec![plain("said before")]);
    draw(&mut tui, 2, "live");

    // The body arrives and the area grows to hold it.
    draw(&mut tui, 6, "body");
    // It ends: one row of summary, and the area is small again.
    tui.insert_history_lines(vec![plain("summary")]);
    draw(&mut tui, 2, "live");

    let rows = tui.terminal().backend().rows();
    let summary = rows
        .iter()
        .position(|row| row == "summary")
        .expect("the summary row");
    let before = rows
        .iter()
        .position(|row| row == "said before")
        .expect("the older row");
    assert_eq!(
        summary,
        before + 1,
        "a gap opened between the history and the summary: {rows:?}"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn history_queued_between_frames_goes_out_with_the_next_one() {
    let mut tui = open(20, 10, 7, 2);
    draw(&mut tui, 2, "live");

    tui.insert_history_lines(vec![plain("finished")]);
    // Nothing is written until a frame, because a row written between two
    // frames lands in the middle of a live area the terminal still believes.
    assert!(
        !tui.terminal()
            .backend()
            .rows()
            .iter()
            .any(|row| row == "finished"),
        "the row went out before the frame did"
    );

    draw(&mut tui, 2, "live");
    assert!(
        tui.terminal()
            .backend()
            .rows()
            .iter()
            .any(|row| row == "finished"),
        "the row never went out"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn history_ends_immediately_above_the_live_area() {
    let mut tui = open(20, 10, 8, 2);
    draw(&mut tui, 2, "live");
    tui.insert_history_lines(vec![plain("last row")]);
    draw(&mut tui, 2, "live");

    let top = tui.terminal().viewport_area.y;
    let rows = tui.terminal().backend().rows();
    assert_eq!(
        rows[usize::from(top) - 1],
        "last row",
        "there is a gap between the history and the live area: {rows:?}"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn queueing_history_asks_for_a_draw() {
    let mut tui = open(20, 8, 6, 2);
    let mut draws = tui.draw_events();

    tui.insert_history_lines(vec![plain("something")]);

    assert!(
        tokio::time::timeout(Duration::from_millis(100), draws.recv())
            .await
            .is_ok(),
        "nothing asked for the draw that writes it"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_window_that_shrank_puts_the_live_area_back_on_the_bottom() {
    let mut tui = open(20, 10, 8, 2);
    draw(&mut tui, 2, "live");

    tui.backend_mut().set_size(20, 6);
    draw(&mut tui, 2, "live");

    let area = tui.terminal().viewport_area;
    assert_eq!(area.bottom(), 6);
    assert_eq!(tui.terminal().backend().row(4), "live0");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn the_transcript_takes_the_whole_window_and_gives_it_back() {
    let mut tui = open(20, 8, 6, 2);
    draw(&mut tui, 2, "live");
    let before = tui.terminal().viewport_area;

    tui.enter_alt_screen().expect("enter");
    assert!(tui.is_alt_screen());
    assert_eq!(
        tui.terminal().viewport_area,
        Rect::new(0, 0, 20, 8),
        "the transcript did not get the window"
    );

    tui.leave_alt_screen().expect("leave");
    assert!(!tui.is_alt_screen());
    assert_eq!(tui.terminal().viewport_area, before);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn queued_history_goes_out_before_the_transcript_takes_the_window() {
    let mut tui = open(20, 10, 7, 2);
    draw(&mut tui, 2, "live");
    tui.insert_history_lines(vec![plain("belongs to the shell")]);

    tui.enter_alt_screen().expect("enter");
    tui.leave_alt_screen().expect("leave");
    draw(&mut tui, 2, "live");

    assert!(
        tui.terminal()
            .backend()
            .rows()
            .iter()
            .any(|row| row == "belongs to the shell"),
        "the row was lost to the overlay"
    );
}

#[test]
fn a_key_release_is_not_a_keystroke() {
    let event = Event::Key(KeyEvent::new_with_kind(
        KeyCode::Char('a'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    ));
    assert_eq!(map_event(event), None);
}

#[test]
fn a_key_press_is_a_keystroke() {
    let event = Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    assert_eq!(map_event(event), Some(TuiEvent::Key(Key::char('a'))));
}

#[test]
fn a_key_repeat_is_a_keystroke() {
    let event = Event::Key(KeyEvent::new_with_kind(
        KeyCode::Enter,
        KeyModifiers::NONE,
        KeyEventKind::Repeat,
    ));
    assert_eq!(
        map_event(event),
        Some(TuiEvent::Key(Key::named(KeyName::Enter)))
    );
}

#[test]
fn a_paste_arrives_in_one_piece() {
    let event = Event::Paste("two\nlines".to_owned());
    assert_eq!(
        map_event(event),
        Some(TuiEvent::Paste("two\nlines".to_owned()))
    );
}

#[test]
fn a_resize_says_how_big_the_window_is_now() {
    assert_eq!(
        map_event(Event::Resize(80, 24)),
        Some(TuiEvent::Resize(Size {
            width: 80,
            height: 24
        }))
    );
}

#[test]
fn nothing_asked_for_a_mouse_event_so_nothing_reacts_to_one() {
    let event = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 1,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(map_event(event), None);
}

#[test]
fn a_focus_change_is_not_this_programs_business() {
    assert_eq!(map_event(Event::FocusGained), None);
    assert_eq!(map_event(Event::FocusLost), None);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_scheduled_frame_wakes_a_loop_that_is_only_waiting() {
    // A draw future dropped on a pending poll deregisters its waker, so the
    // notification that arrives a frame later wakes nothing. The draw is then
    // only noticed when something else happens to poll the stream again —
    // which on an idle prompt is the next keystroke. So the thing to assert is
    // not that the draw arrives but that it arrives *when it was sent*.
    let tui = open(20, 8, 6, 2);
    let mut draws = tui.draw_stream();
    tui.frame_requester().schedule_frame();

    let started = tokio::time::Instant::now();
    let woken = tokio::time::timeout(Duration::from_mins(1), draws.next()).await;
    let waited = started.elapsed();

    assert_eq!(woken.ok().flatten(), Some(TuiEvent::Draw));
    assert!(
        waited < Duration::from_millis(500),
        "the draw waited {waited:?} for something else to poll the stream"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn every_frame_after_the_first_wakes_it_too() {
    let tui = open(20, 8, 6, 2);
    let mut draws = tui.draw_stream();

    for at in 0..3 {
        tui.frame_requester().schedule_frame();
        let started = tokio::time::Instant::now();
        let woken = tokio::time::timeout(Duration::from_mins(1), draws.next()).await;
        let waited = started.elapsed();

        assert_eq!(woken.ok().flatten(), Some(TuiEvent::Draw));
        assert!(
            waited < Duration::from_millis(500),
            "frame {at} waited {waited:?}"
        );
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_terminal_that_loses_scrolled_rows_repaints_instead() {
    // Windows Terminal discards rows scrolled out of a partial region rather
    // than moving them into its scrollback, so the whole screen scrolls and
    // the live area is painted again.
    let backend = VT100Backend::new(20, 10);
    let mut terminal = Terminal::anchored(
        backend,
        Size {
            width: 20,
            height: 10,
        },
        Position { x: 0, y: 4 },
    );
    terminal.set_viewport_area(Rect::new(0, 4, 20, 2));
    let mut tui = Tui::new(terminal, InsertMode::Repaint);

    draw(&mut tui, 2, "live");
    tui.insert_history_lines(vec![plain("history")]);
    draw(&mut tui, 2, "live");

    let rows = tui.terminal().backend().rows();
    assert!(
        rows.iter().any(|row| row == "history"),
        "nothing was written: {rows:?}"
    );
    assert!(rows.iter().any(|row| row == "live0"));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_live_area_that_grew_on_a_repainting_terminal_still_fits() {
    let backend = VT100Backend::new(20, 8);
    let mut terminal = Terminal::anchored(
        backend,
        Size {
            width: 20,
            height: 8,
        },
        Position { x: 0, y: 5 },
    );
    terminal.set_viewport_area(Rect::new(0, 5, 20, 3));
    let mut tui = Tui::new(terminal, InsertMode::Repaint);

    draw(&mut tui, 3, "live");
    draw(&mut tui, 6, "live");

    let area = tui.terminal().viewport_area;
    assert_eq!(area.height, 6);
    assert_eq!(area.bottom(), 8, "the live area hangs off the screen");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn giving_the_terminal_back_leaves_the_cursor_under_the_conversation() {
    let mut tui = open(20, 8, 5, 3);
    draw(&mut tui, 3, "live");

    tui.restore();

    let rows = tui.terminal().backend().rows();
    assert!(
        !rows.iter().any(|row| row.starts_with("live")),
        "the live area was left on the screen: {rows:?}"
    );
    assert_eq!(tui.terminal().backend().cursor(), (5, 0));
    assert!(tui.terminal().backend().cursor_visible());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn giving_the_terminal_back_twice_is_the_same_as_once() {
    let mut tui = open(20, 8, 5, 3);
    draw(&mut tui, 3, "live");

    tui.restore();
    let after = tui.terminal().backend().rows();
    tui.restore();

    assert_eq!(tui.terminal().backend().rows(), after);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn nothing_is_drawn_after_the_terminal_is_given_back() {
    let mut tui = open(20, 8, 5, 3);
    tui.restore();
    let after = tui.terminal().backend().rows();

    draw(&mut tui, 3, "late");

    assert_eq!(tui.terminal().backend().rows(), after);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn entering_the_transcript_twice_changes_nothing() {
    let mut tui = open(20, 8, 6, 2);
    draw(&mut tui, 2, "live");

    tui.enter_alt_screen().expect("enter");
    let area = tui.terminal().viewport_area;
    tui.enter_alt_screen().expect("enter again");

    assert_eq!(tui.terminal().viewport_area, area);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn leaving_a_transcript_that_was_never_opened_changes_nothing() {
    let mut tui = open(20, 8, 6, 2);
    draw(&mut tui, 2, "live");
    let area = tui.terminal().viewport_area;

    tui.leave_alt_screen().expect("leave");

    assert_eq!(tui.terminal().viewport_area, area);
    assert!(!tui.is_alt_screen());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn the_transcript_draws_over_the_whole_window() {
    let mut tui = open(20, 8, 6, 2);
    draw(&mut tui, 2, "live");
    tui.enter_alt_screen().expect("enter");

    draw(&mut tui, 8, "page");

    assert_eq!(tui.terminal().viewport_area, Rect::new(0, 0, 20, 8));
    assert_eq!(tui.terminal().backend().row(0), "page0");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn queueing_nothing_asks_for_nothing() {
    let mut tui = open(20, 8, 6, 2);
    draw(&mut tui, 2, "live");
    let before = tui.terminal().backend().rows();

    tui.insert_history_lines(Vec::new());
    draw(&mut tui, 2, "live");

    assert_eq!(tui.terminal().backend().rows(), before);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn asking_for_everything_to_be_painted_again_paints_it_again() {
    let mut tui = open(20, 8, 6, 2);
    draw(&mut tui, 2, "live");

    // Damage nothing announces: another program wrote to the same terminal.
    write!(tui.backend_mut(), "\x1b[7;1Hnoise").expect("damage");
    assert_eq!(tui.terminal().backend().row(6), "noise");

    tui.invalidate();
    draw(&mut tui, 2, "live");

    assert_eq!(tui.terminal().backend().row(6), "live0");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn the_first_frame_makes_room_for_itself_at_the_foot_of_the_screen() {
    // What a freshly opened terminal looks like: no live area yet, at the
    // bottom. The first draw has to scroll the shell's screen up rather than
    // drawing over it or waiting to be told where the cursor is.
    let backend = VT100Backend::new(20, 8);
    let mut terminal = Terminal::new(backend).expect("open");
    // Something the shell left behind, on the row its prompt would be on.
    write!(terminal.backend_mut(), "\x1b[4;1H$ darkwire chat").expect("prefill");
    let mut tui = Tui::new(terminal, InsertMode::ScrollRegion);

    draw(&mut tui, 3, "live");

    let rows = tui.terminal().backend().rows();
    assert_eq!(
        rows[5], "live0",
        "the live area is not at the foot: {rows:?}"
    );
    assert_eq!(rows[7], "live2");
    // Scrolled up to make room, the way printing three lines would have, and
    // not drawn over.
    assert_eq!(
        rows[0], "$ darkwire chat",
        "the shell's screen was lost: {rows:?}"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_window_rebuilt_for_a_resize_keeps_nothing_it_had_drawn() {
    // A terminal that reflows its screen when the width changes moves the rows
    // this program drew to wherever the new width puts them, and leaves a copy
    // of the live area behind for every step of a drag. Nothing can address
    // those rows afterwards, so the window is erased outright.
    let mut tui = open(20, 8, 5, 3);
    draw(&mut tui, 3, "live");
    assert!(
        tui.terminal()
            .backend()
            .rows()
            .iter()
            .any(|row| row == "live0")
    );

    tui.reset_screen().expect("reset");

    let rows = tui.terminal().backend().rows();
    assert_eq!(rows.join(""), "", "something survived the reset: {rows:?}");
    assert_eq!(tui.terminal().viewport_area.height, 0);
    assert_eq!(tui.terminal().viewport_area.y, 8);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn the_live_area_comes_back_at_the_foot_after_a_reset() {
    let mut tui = open(20, 8, 5, 3);
    draw(&mut tui, 3, "live");
    tui.reset_screen().expect("reset");

    draw(&mut tui, 2, "again");

    let rows = tui.terminal().backend().rows();
    assert_eq!(rows[6], "again0");
    assert_eq!(rows[7], "again1");
    assert!(
        !rows[..6].iter().any(|row| row.starts_with("live")),
        "a copy of the old live area is still there: {rows:?}"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_line_taken_back_off_the_composer_and_typed_again_costs_the_history_nothing() {
    // Shift-Return, then a change of mind, then Shift-Return again. Growing
    // used to scroll the history up every time, and shrinking left the rows it
    // gave back empty above the live area — so the conversation walked one row
    // up the screen for every line that came and went, and never came back.
    //
    // The blank rows a shrink leaves are exactly the room the next growth
    // needs. Taking them back is free; scrolling is not, because a row that
    // has gone into the scrollback cannot be brought down again.
    let mut tui = open(20, 8, 6, 2);
    draw(&mut tui, 2, "live");
    tui.insert_history_lines(vec![plain("older"), plain("newer")]);
    draw(&mut tui, 2, "live");

    let before: Vec<String> = tui.terminal().backend().rows();
    let at_first = before
        .iter()
        .position(|row| row == "newer")
        .expect("the history is on the screen");

    // A line arrives, then goes, then arrives again.
    draw(&mut tui, 3, "live");
    draw(&mut tui, 2, "live");
    draw(&mut tui, 3, "live");

    let rows = tui.terminal().backend().rows();
    let at_last = rows
        .iter()
        .position(|row| row == "newer")
        .expect("the history left the screen");
    assert_eq!(
        at_last,
        at_first.saturating_sub(1),
        "the history moved more than the one row the taller area needs: {rows:?}"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_live_area_that_shrank_gives_back_its_bottom_rows_and_keeps_its_top() {
    // Taking a line back off the composer. The live area used to stay glued to
    // the bottom of the screen, so its *top* dropped — which pushed the box
    // away from the conversation and opened a gap above it that nothing ever
    // filled. Growing and shrinking have to be each other's opposite: a line
    // arrives at the bottom, so a line leaves from the bottom.
    let mut tui = open(20, 8, 6, 2);
    draw(&mut tui, 2, "live");
    tui.insert_history_lines(vec![plain("older"), plain("newer")]);
    draw(&mut tui, 2, "live");

    // Two lines arrive, then one goes.
    draw(&mut tui, 4, "live");
    let grown = tui.terminal().viewport_area;
    draw(&mut tui, 3, "live");
    let shrunk = tui.terminal().viewport_area;

    assert_eq!(shrunk.top(), grown.top(), "the top of the box moved");
    assert_eq!(shrunk.height, 3);

    // And the row it gave back is blank rather than holding the old frame.
    let rows = tui.terminal().backend().rows();
    assert_eq!(rows[usize::from(shrunk.bottom())], "", "{rows:?}");
}
