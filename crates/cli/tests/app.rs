//! The prompt's one loop, driven over a terminal that exists in memory.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a fixture that does not hold is a failing test either way"
)]

use std::time::Duration;

use darkwire::app::TuiSurface;
use darkwire::chat::Surface;
use darkwire::i18n::Env;
use darkwire::program::{ChatArgs, Globals};
use darkwire_tui::insert_history::InsertMode;
use darkwire_tui::terminal::Terminal;
use darkwire_tui::testkit::VT100Backend;
use darkwire_tui::tui::{Tui, TuiEvent};
use darkwire_tui::{Key, KeyName};
use ratatui::layout::{Position, Rect, Size};
use tokio::sync::mpsc;

/// A session with its state under a temporary home.
/// The answer a menu gives for a row that was simply chosen.
fn chose(row: usize) -> darkwire::pickers::MenuAnswer {
    darkwire::pickers::MenuAnswer { row, action: None }
}

fn session(home: &tempfile::TempDir) -> darkwire::chat::ChatSession {
    darkwire::chat::open(
        &Globals {
            home: Some(home.path().display().to_string()),
            color: Some(false),
            ..Globals::default()
        },
        &ChatArgs {
            workspaces: Some(home.path().join("workspaces").display().to_string()),
            ..ChatArgs::default()
        },
        &Env::empty(),
    )
    .unwrap()
}

/// A prompt over an emulator, and the channel its events arrive on.
fn prompt(
    session: &darkwire::chat::ChatSession,
) -> (TuiSurface<VT100Backend>, mpsc::UnboundedSender<TuiEvent>) {
    let backend = VT100Backend::new(80, 24);
    let mut terminal = Terminal::anchored(
        backend,
        Size {
            width: 80,
            height: 24,
        },
        Position { x: 0, y: 20 },
    );
    terminal.set_viewport_area(Rect::new(0, 20, 80, 4));
    let tui = Tui::new(terminal, InsertMode::ScrollRegion);

    let mut surface = TuiSurface::over(tui, session);
    let (events, rx) = mpsc::unbounded_channel();
    surface.listen_to(Box::pin(tokio_stream_from(rx)));
    (surface, events)
}

/// A receiver as a stream that never ends.
///
/// Never ending matters: a `select!` arm whose stream finished is an arm that
/// is always ready with `None`, which would spin the loop.
fn tokio_stream_from(
    mut rx: mpsc::UnboundedReceiver<TuiEvent>,
) -> impl futures::Stream<Item = TuiEvent> + Send {
    futures::stream::poll_fn(move |context| match rx.poll_recv(context) {
        std::task::Poll::Ready(None) => std::task::Poll::Pending,
        other => other,
    })
}

/// Types a line and presses Return.
fn types(events: &mpsc::UnboundedSender<TuiEvent>, text: &str) {
    for character in text.chars() {
        events.send(TuiEvent::Key(Key::char(character))).unwrap();
    }
    events
        .send(TuiEvent::Key(Key::named(KeyName::Enter)))
        .unwrap();
}

/// Everything the emulator is showing.
fn screen(surface: &TuiSurface<VT100Backend>) -> Vec<String> {
    surface.tui().terminal().backend().rows()
}

#[tokio::test]
async fn a_typed_line_comes_back_from_the_prompt() {
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (mut surface, events) = prompt(&session);

    types(&events, "hello");

    let line = tokio::time::timeout(Duration::from_secs(5), surface.next_line())
        .await
        .expect("the prompt answered");
    assert_eq!(line.as_deref(), Some("hello"));
}

#[tokio::test]
async fn an_interrupt_at_an_idle_prompt_leaves() {
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (mut surface, events) = prompt(&session);

    events.send(TuiEvent::Key(Key::ctrl('c'))).unwrap();

    let line = tokio::time::timeout(Duration::from_secs(5), surface.next_line())
        .await
        .expect("the prompt answered");
    assert_eq!(line, None);
}

#[tokio::test]
async fn what_was_said_reaches_the_screen() {
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (mut surface, events) = prompt(&session);

    surface.echo("a question").await;
    types(&events, "the next one");
    let _ = tokio::time::timeout(Duration::from_secs(5), surface.next_line()).await;

    let rows = screen(&surface);
    assert!(
        rows.iter().any(|row| row.contains("a question")),
        "the message is not on the screen: {rows:?}"
    );
}

#[tokio::test]
async fn a_message_typed_while_a_turn_runs_waits_rather_than_being_lost() {
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (mut surface, events) = prompt(&session);
    let token = tokio_util::sync::CancellationToken::new();

    let turn = Box::pin(async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(darkwire::chat::TurnOutcome {
            stop_reason: darkwire_protocol::StopReason::Complete,
            aborted: false,
            failed: false,
        })
    });
    types(&events, "while it runs");
    let _ = tokio::time::timeout(Duration::from_secs(5), surface.run(&token, turn)).await;

    // The queue is what `next_line` takes first, without waiting for a key.
    let line = tokio::time::timeout(Duration::from_secs(5), surface.next_line())
        .await
        .expect("the prompt answered");
    assert_eq!(line.as_deref(), Some("while it runs"));
}

#[tokio::test]
async fn an_interrupt_during_a_turn_cancels_it_rather_than_leaving() {
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (mut surface, events) = prompt(&session);
    let token = tokio_util::sync::CancellationToken::new();

    let cancelled = token.clone();
    let turn = Box::pin(async move {
        cancelled.cancelled().await;
        Ok(darkwire::chat::TurnOutcome {
            stop_reason: darkwire_protocol::StopReason::Aborted,
            aborted: true,
            failed: false,
        })
    });
    events.send(TuiEvent::Key(Key::ctrl('c'))).unwrap();

    let outcome = tokio::time::timeout(Duration::from_secs(5), surface.run(&token, turn))
        .await
        .expect("the turn ended");
    assert!(outcome.unwrap().aborted);
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn a_picker_opened_by_a_command_answers_while_the_command_waits() {
    // The whole reason `attend` exists: a menu only answers because something
    // is still reading the keyboard, and that something is this loop.
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (mut surface, events) = prompt(&session);

    let menu = surface.menu();
    let command = Box::pin(async move {
        let request = darkwire::pickers::MenuRequest {
            items: vec![
                darkwire_tui::SelectItem::new(0, "first"),
                darkwire_tui::SelectItem::new(1, "second"),
            ],
            labels: darkwire_tui::SelectLabels {
                title: "pick one".to_owned(),
                empty: "nothing".to_owned(),
                footer: "enter to choose".to_owned(),
                filter_prefix: None,
            },
            index: None,
            placement: darkwire::pickers::Placement::Prompt,
            actions: Vec::new(),
        };
        let chosen = menu.choose(request).await;
        assert_eq!(chosen, Some(chose(1)));
        darkwire::chat::Flow::Again
    });

    // After the menu is on the pane, not before: a key that arrives first
    // reaches the composer, which is what it should do.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        events
            .send(TuiEvent::Key(Key::named(KeyName::Down)))
            .unwrap();
        events
            .send(TuiEvent::Key(Key::named(KeyName::Enter)))
            .unwrap();
    });

    let flow = tokio::time::timeout(Duration::from_secs(5), surface.attend(command))
        .await
        .expect("the command finished");
    assert!(matches!(flow, darkwire::chat::Flow::Again));
}

#[tokio::test]
async fn a_window_placed_menu_takes_the_screen_and_still_answers() {
    // The sessions list is as long as the install is old and is filtered by
    // typing, so five rows under the composer is a filter applied blind. It
    // takes the window the way a listing does, and the prompt underneath is
    // gone rather than half visible.
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (mut surface, events) = prompt(&session);

    let menu = surface.menu();
    let command = Box::pin(async move {
        let request = darkwire::pickers::MenuRequest {
            items: vec![
                darkwire_tui::SelectItem::new(0, "yesterday's question"),
                darkwire_tui::SelectItem::new(1, "the one before that"),
            ],
            labels: darkwire_tui::SelectLabels {
                title: "Which session?".to_owned(),
                empty: "nothing".to_owned(),
                footer: "enter to choose".to_owned(),
                filter_prefix: None,
            },
            index: None,
            placement: darkwire::pickers::Placement::Window,
            actions: Vec::new(),
        };
        assert_eq!(menu.choose(request).await, Some(chose(1)));
        darkwire::chat::Flow::Again
    });

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Filters down to the second row, which is the thing five rows under a
        // composer cannot do for a list of two hundred.
        for character in ['b', 'e', 'f', 'o', 'r', 'e'] {
            events.send(TuiEvent::Key(Key::char(character))).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        events
            .send(TuiEvent::Key(Key::named(KeyName::Enter)))
            .unwrap();
    });

    let flow = tokio::time::timeout(Duration::from_secs(5), surface.attend(command))
        .await
        .expect("the command finished");
    assert!(matches!(flow, darkwire::chat::Flow::Again));
}

#[tokio::test]
async fn a_picker_with_no_loop_behind_it_answers_nothing() {
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (surface, _events) = prompt(&session);
    let menu = surface.menu();
    drop(surface);

    let request = darkwire::pickers::MenuRequest {
        items: vec![darkwire_tui::SelectItem::new(0, "first")],
        labels: darkwire_tui::SelectLabels {
            title: "pick one".to_owned(),
            empty: "nothing".to_owned(),
            footer: "enter to choose".to_owned(),
            filter_prefix: None,
        },
        index: None,
        placement: darkwire::pickers::Placement::Prompt,
        actions: Vec::new(),
    };
    assert_eq!(menu.choose(request).await, None);
}

#[tokio::test]
async fn a_verb_answers_and_the_menu_can_be_opened_again() {
    // What a verb costs, and the loop that pays it. The caller applies the verb
    // and asks for the menu back, so the overlay is gone between the two. The
    // second buffer is given back in `draw` rather than when an overlay closes,
    // so a close and a re-open inside one frame never flashes the conversation
    // up and takes it away again. Counted rather than looked at: both the one
    // handover and the flicker leave the same screen behind.
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (mut surface, events) = prompt(&session);

    let menu = surface.menu();
    let command = Box::pin(async move {
        for round in 0..2 {
            let request = darkwire::pickers::MenuRequest {
                items: vec![darkwire_tui::SelectItem::new(0, "a task")],
                labels: darkwire_tui::SelectLabels {
                    title: "Which task?".to_owned(),
                    empty: "nothing".to_owned(),
                    footer: "enter to choose".to_owned(),
                    filter_prefix: None,
                },
                index: None,
                placement: darkwire::pickers::Placement::Window,
                actions: vec![darkwire_tui::SelectAction {
                    chord: 'x',
                    label: "delete".to_owned(),
                }],
            };
            let answer = menu.choose(request).await;
            assert_eq!(
                answer,
                Some(darkwire::pickers::MenuAnswer {
                    row: 0,
                    action: Some(0),
                }),
                "round {round}"
            );
        }
        darkwire::chat::Flow::Again
    });

    tokio::spawn(async move {
        for _ in 0..2 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            events.send(TuiEvent::Key(Key::ctrl('x'))).unwrap();
        }
    });

    let flow = tokio::time::timeout(Duration::from_secs(5), surface.attend(command))
        .await
        .expect("the command finished");

    assert!(matches!(flow, darkwire::chat::Flow::Again));
    assert!(
        !surface.tui().is_alt_screen(),
        "the window was not given back once nothing was over the prompt"
    );
    assert_eq!(
        surface.tui().alt_screen_leaves(),
        1,
        "the window went back and forth between the two openings"
    );
}

#[tokio::test]
async fn the_transcript_takes_the_window_and_gives_it_back() {
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (mut surface, events) = prompt(&session);

    surface.echo("something to read").await;
    events.send(TuiEvent::Key(Key::ctrl('t'))).unwrap();
    events
        .send(TuiEvent::Key(Key::named(KeyName::Escape)))
        .unwrap();
    types(&events, "back to typing");

    let line = tokio::time::timeout(Duration::from_secs(5), surface.next_line())
        .await
        .expect("the prompt answered");
    assert_eq!(line.as_deref(), Some("back to typing"));
    assert!(
        !surface.tui().is_alt_screen(),
        "the window was not given back"
    );
}

#[tokio::test]
async fn closing_puts_the_terminal_back() {
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (mut surface, _events) = prompt(&session);

    surface.echo("said before leaving").await;
    surface.close().await;

    assert!(
        screen(&surface)
            .iter()
            .any(|row| row.contains("said before leaving")),
        "what was said did not reach the scrollback"
    );
}

#[tokio::test]
async fn a_resize_lays_the_prompt_out_again() {
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (mut surface, events) = prompt(&session);

    events
        .send(TuiEvent::Resize(Size {
            width: 40,
            height: 12,
        }))
        .unwrap();
    types(&events, "after the resize");

    let line = tokio::time::timeout(Duration::from_secs(5), surface.next_line())
        .await
        .expect("the prompt answered");
    assert_eq!(line.as_deref(), Some("after the resize"));
}

#[tokio::test]
async fn a_paste_lands_on_the_composer_as_one_line() {
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (mut surface, events) = prompt(&session);

    events
        .send(TuiEvent::Paste("two\nlines".to_owned()))
        .unwrap();
    events
        .send(TuiEvent::Key(Key::named(KeyName::Enter)))
        .unwrap();

    let line = tokio::time::timeout(Duration::from_secs(5), surface.next_line())
        .await
        .expect("the prompt answered");
    assert_eq!(
        line.as_deref(),
        Some("two lines"),
        "a pasted newline submitted something nobody asked for"
    );
}

#[tokio::test]
async fn moving_to_another_conversation_writes_that_ones_name() {
    // The header names the session. Read from what the surface was already
    // holding, it would name the conversation that was just left.
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (mut surface, _events) = prompt(&session);

    surface.echo("in the first one").await;
    let joined = darkwire::header::HeaderView {
        session: "the-one-joined".to_owned(),
        ..darkwire::header::HeaderView::default()
    };
    surface.reopen(&joined, &[]).await;
    surface.close().await;

    let rows = screen(&surface);
    assert!(
        rows.iter().any(|row| row.contains("the-one-joined")),
        "the header does not name the conversation being joined: {rows:?}"
    );
}

#[tokio::test]
async fn the_transcript_command_opens_the_same_window_the_key_does() {
    // `/transcript` for somebody who does not know `ctrl-t`. Reasoning and
    // what a turn cost are in there whatever the switches say, which is the
    // whole reason the command is worth having.
    let home = tempfile::tempdir().unwrap();
    let session = session(&home);
    let (mut surface, events) = prompt(&session);

    surface.echo("something to read").await;
    let opened = surface.transcript().await;
    assert!(opened, "the window was not taken");
    assert!(surface.tui().is_alt_screen());

    events
        .send(TuiEvent::Key(Key::named(KeyName::Escape)))
        .unwrap();
    types(&events, "back to typing");
    let line = tokio::time::timeout(Duration::from_secs(5), surface.next_line())
        .await
        .expect("the prompt answered");

    assert_eq!(line.as_deref(), Some("back to typing"));
    assert!(!surface.tui().is_alt_screen(), "the window came back");
}
