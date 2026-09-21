//! The conversation the terminal is left holding, over a real emulator.
//!
//! The widget's own tests read the rows it hands over. This drives those rows
//! through the live area and into a vt100 screen, which is the only place the
//! two can disagree: a row the widget never wrote is still a row the terminal
//! is showing, and a live area that gives back more rows than it fills leaves
//! them blank in the middle of the conversation for ever.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a fixture that does not hold is a failing test either way"
)]

use darkwire::chat_widget::ChatWidget;
use darkwire::history_cell::FoldLabels;
use darkwire::render::{LineKind, TranscriptEvent};
use darkwire_tui::insert_history::InsertMode;
use darkwire_tui::terminal::Terminal;
use darkwire_tui::testkit::VT100Backend;
use darkwire_tui::tui::Tui;
use ratatui::layout::{Position, Rect, Size};

const WIDTH: u16 = 80;
const HEIGHT: u16 = 24;

fn widget() -> ChatWidget {
    let mut widget = ChatWidget::new(
        darkwire_tui::theme_for(Some(false)),
        FoldLabels {
            thinking: "thinking".to_owned(),
            reasoning: "reasoning".to_owned(),
            too_small: "window too small".to_owned(),
        },
        "generating",
    );
    widget.set_screen_size(WIDTH, HEIGHT);
    widget
}

fn tui() -> Tui<VT100Backend> {
    let backend = VT100Backend::new(WIDTH, HEIGHT);
    let mut terminal = Terminal::anchored(
        backend,
        Size {
            width: WIDTH,
            height: HEIGHT,
        },
        Position {
            x: 0,
            y: HEIGHT - 2,
        },
    );
    terminal.set_viewport_area(Rect::new(0, HEIGHT - 2, WIDTH, 2));
    Tui::new(terminal, InsertMode::ScrollRegion)
}

/// One frame, the way the prompt's loop draws one.
fn draw(tui: &mut Tui<VT100Backend>, widget: &mut ChatWidget) {
    let height = widget.desired_height(WIDTH);
    let pending = widget.drain_history();
    tui.insert_history_lines(pending);
    tui.draw(height, |frame| {
        widget.render(frame.area, frame.buffer);
    })
    .expect("draw");
}

fn screen(tui: &Tui<VT100Backend>) -> Vec<String> {
    tui.terminal().backend().rows()
}

/// The rows of the conversation, from the top down to the live area.
fn history(tui: &Tui<VT100Backend>) -> Vec<String> {
    let top = tui.terminal().viewport_area.top();
    screen(tui).into_iter().take(usize::from(top)).collect()
}

#[tokio::test(flavor = "current_thread")]
async fn a_folded_tool_call_leaves_no_hole_where_its_body_was() {
    // The body grows the live area while it streams and one summary row is all
    // that reaches the scrollback. The rows the area gives back are history
    // now, and history nothing ever wrote into is a blank gap the reader
    // scrolls past. It used to be as tall as the output the call produced.
    let mut tui = tui();
    let mut widget = widget();

    widget.echo("what memories do you have?");
    draw(&mut tui, &mut widget);

    widget.start_turn();
    widget.handle_event(&TranscriptEvent::Line {
        kind: LineKind::ToolCall,
        text: "⚙ memory".to_owned(),
    });
    widget.handle_event(&TranscriptEvent::ToolBodyStart {
        summary: "  ✓ 1ms".to_owned(),
    });
    for at in 0..12 {
        widget.handle_event(&TranscriptEvent::Line {
            kind: LineKind::Notice,
            text: format!("    line {at} of what it said"),
        });
        draw(&mut tui, &mut widget);
    }
    widget.handle_event(&TranscriptEvent::ToolBodyEnd);
    draw(&mut tui, &mut widget);

    widget.handle_event(&TranscriptEvent::AssistantDelta {
        text: "So, honestly, here is the answer.\n".to_owned(),
        depth: 0,
    });
    widget.end_turn();
    draw(&mut tui, &mut widget);

    let rows = history(&tui);
    let first = rows
        .iter()
        .position(|row| !row.is_empty())
        .expect("something was said");
    let last = rows
        .iter()
        .rposition(|row| !row.is_empty())
        .expect("something was said");
    let blanks = rows[first..=last]
        .iter()
        .filter(|row| row.is_empty())
        .count();
    assert!(
        blanks <= 1,
        "{blanks} blank rows in the middle of the conversation: {rows:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_run_of_reasoning_that_folded_away_leaves_no_hole_either() {
    // Same shape, and the one the report was about: the reasoning is collapsed
    // to a row, so everything it said is a row the live area held and gave
    // back.
    let mut tui = tui();
    let mut widget = widget();

    widget.echo("what memories do you have?");
    draw(&mut tui, &mut widget);

    widget.start_turn();
    widget.handle_event(&TranscriptEvent::ReasoningStart { elapsed_ms: None });
    for at in 0..10 {
        widget.handle_event(&TranscriptEvent::ReasoningDelta {
            text: format!("weighing it up, part {at}\n"),
            depth: 0,
        });
        draw(&mut tui, &mut widget);
    }
    widget.handle_event(&TranscriptEvent::ReasoningEnd);
    draw(&mut tui, &mut widget);

    widget.handle_event(&TranscriptEvent::AssistantDelta {
        text: "Good question.\n".to_owned(),
        depth: 0,
    });
    widget.end_turn();
    draw(&mut tui, &mut widget);

    let rows = history(&tui);
    let first = rows
        .iter()
        .position(|row| !row.is_empty())
        .expect("something was said");
    let last = rows
        .iter()
        .rposition(|row| !row.is_empty())
        .expect("something was said");
    let blanks = rows[first..=last]
        .iter()
        .filter(|row| row.is_empty())
        .count();
    assert!(
        blanks <= 1,
        "{blanks} blank rows in the middle of the conversation: {rows:?}"
    );
}
