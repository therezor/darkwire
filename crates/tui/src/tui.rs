//! The program's claim on the terminal, and everything that follows from it.
//!
//! One of these exists while the prompt is open. It owns raw mode, the live
//! area at the bottom of the screen, the queue of rows waiting to go to the
//! scrollback, and the single stream of things the loop reacts to. Nothing
//! else writes to the terminal.
//!
//! **No alternate screen, except to show the transcript.** A program that
//! takes the second buffer owns the whole window, and owning the window means
//! implementing scrolling, selection and search itself, badly, for something
//! the terminal already does. It also means the conversation vanishes on exit.
//! The one thing worth the second buffer is a pager over the whole transcript,
//! which is a different kind of screen and says so by taking one.
//!
//! **No mouse capture.** A terminal only reports mouse events to a program
//! that asks, and a program that asks is one the terminal stops selecting for.
//! Nothing here asks.
//!
//! The rest is generic over the backend on purpose: [`Tui::init`] is the only
//! function that touches the real terminal, and it is short. Everything with a
//! decision in it — how the live area grows, when history is flushed, what a
//! resize does — is driven by tests over a terminal emulator that parses the
//! same bytes a terminal would.

use std::io::{self, Write};
use std::pin::Pin;

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, KeyEvent, KeyEventKind,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{execute, queue};
use futures::{Stream, StreamExt};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::{Position, Rect, Size};
use ratatui::text::Line;
use tokio::sync::broadcast;

use crate::frame::{FrameRequester, scheduler};
use crate::insert_history::{InsertMode, insert_history_lines};
use crate::keys::Key;
use crate::terminal::{Frame, Terminal};

/// Something the loop has to react to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiEvent {
    /// A keystroke.
    Key(Key),
    /// Text the terminal pasted in one go.
    Paste(String),
    /// The window changed size.
    Resize(Size),
    /// The screen is out of date.
    Draw,
}

/// The terminal over the real screen.
pub type StdoutTui = Tui<CrosstermBackend<io::Stdout>>;

/// The live area, the scrollback queue and the event stream.
pub struct Tui<B>
where
    B: Backend<Error = io::Error> + Write,
{
    terminal: Terminal<B>,
    requester: FrameRequester,
    draws: broadcast::Sender<()>,
    /// Rows finished but not yet handed to the terminal. They go out inside
    /// the next draw, because a row written between two frames would land in
    /// the middle of a live area the terminal still believes is there.
    pending: Vec<Line<'static>>,
    mode: InsertMode,
    alt_screen: bool,
    /// How many times the window has been given back.
    ///
    /// Counted because "it is given back once" and "it is given back and taken
    /// again on every keystroke" leave the same screen behind, and only one of
    /// them is a prompt that does not flicker.
    alt_screen_leaves: usize,
    /// Blank rows sitting directly above the live area, waiting to be used.
    ///
    /// A live area that shrinks stays on the bottom, so its top moves *down*
    /// and the rows it gives back are above it: they are history now, and
    /// nothing has ever written into them. The next rows that go out fill them
    /// before they scroll anything, because history written over the gap is
    /// history with no hole in it.
    reclaim: u16,
    saved_viewport: Option<Rect>,
    /// Whether this took raw mode and so has to give it back.
    ///
    /// Only the one built over the real terminal did. A terminal in memory has
    /// no modes to restore, and calling for them would reach past the backend
    /// to whatever tty the tests happen to be running under.
    owns_modes: bool,
    restored: bool,
}

impl StdoutTui {
    /// Takes raw mode and opens a live area at the bottom of the screen.
    ///
    /// # Errors
    ///
    /// When raw mode or the terminal's size cannot be had, in which case
    /// nothing has been taken and the caller can fall back to a plain prompt.
    pub fn init() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut out = io::stdout();
        if let Err(error) = execute!(out, EnableBracketedPaste) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        // Without these, a terminal sends the same byte for Return and for
        // Shift-Return, so one of them cannot mean "a new line in what I am
        // writing". Old terminals do not have them and say so by failing,
        // which is not a reason to refuse to start: the composer keeps
        // Alt-Return and Ctrl-J, which need nothing.
        let _ = execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
        // The release profile aborts rather than unwinding, and a hook runs
        // before the abort where a `Drop` does not. A panic that left raw mode
        // on leaves a shell nobody can type into.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            previous(info);
        }));

        let terminal = match Terminal::new(CrosstermBackend::new(out)) {
            Ok(terminal) => terminal,
            Err(error) => {
                restore_terminal();
                return Err(error);
            }
        };
        let mut tui = Tui::new(terminal, detect_insert_mode());
        tui.owns_modes = true;
        Ok(tui)
    }

    /// Keystrokes, pastes, resizes and draw notifications, in one stream.
    #[must_use]
    pub fn event_stream(&self) -> Pin<Box<dyn Stream<Item = TuiEvent> + Send>> {
        let input = crossterm::event::EventStream::new()
            .filter_map(|event| async move { event.ok().and_then(map_event) });
        Box::pin(futures::stream::select(
            input,
            draw_stream(self.draws.subscribe()),
        ))
    }
}

impl<B> Tui<B>
where
    B: Backend<Error = io::Error> + Write,
{
    /// Wraps a terminal that is already open.
    pub fn new(terminal: Terminal<B>, mode: InsertMode) -> Self {
        let (requester, draws) = scheduler();
        Self {
            terminal,
            requester,
            draws,
            pending: Vec::new(),
            reclaim: 0,
            mode,
            alt_screen: false,
            alt_screen_leaves: 0,
            saved_viewport: None,
            owns_modes: false,
            restored: false,
        }
    }

    /// A handle anything can hold to ask for a draw.
    #[must_use]
    pub fn frame_requester(&self) -> FrameRequester {
        self.requester.clone()
    }

    /// Draw notifications on their own, for a test with no keyboard.
    #[must_use]
    pub fn draw_events(&self) -> broadcast::Receiver<()> {
        self.draws.subscribe()
    }

    /// Draw notifications as the stream the loop merges them from.
    ///
    /// The same construction [`StdoutTui::event_stream`] uses, so a test of
    /// "does a scheduled frame reach a loop that is waiting" is a test of the
    /// thing that runs.
    #[must_use]
    pub fn draw_stream(&self) -> Pin<Box<dyn Stream<Item = TuiEvent> + Send>> {
        draw_stream(self.draws.subscribe())
    }

    /// The terminal under the live area.
    pub fn terminal(&self) -> &Terminal<B> {
        &self.terminal
    }

    /// The backend, for something that has to write escape sequences itself.
    ///
    /// Everything that draws goes through [`Tui::draw`]; this is for the two
    /// things that cannot, which are a test standing in for the window and the
    /// transcript overlay measuring the screen it is about to take.
    pub fn backend_mut(&mut self) -> &mut B {
        self.terminal.backend_mut()
    }

    /// What the backend says the screen is.
    ///
    /// # Errors
    ///
    /// Whatever the backend gives back.
    pub fn size(&self) -> io::Result<Size> {
        self.terminal.size()
    }

    /// Queues rows for the scrollback and asks for the draw that writes them.
    pub fn insert_history_lines(&mut self, lines: Vec<Line<'static>>) {
        if lines.is_empty() {
            return;
        }
        self.pending.extend(lines);
        self.requester.schedule_frame();
    }

    /// Makes the next draw write every cell.
    pub fn invalidate(&mut self) {
        self.terminal.invalidate();
    }

    /// Erases the window and puts the live area back at the foot of it.
    ///
    /// What a resize needs. A terminal that reflows its screen when the width
    /// changes moves the rows this program drew to wherever the new width puts
    /// them, and leaves a copy of the live area behind for every step of a
    /// drag. Nothing can address those rows afterwards, so the window is
    /// erased and whatever should be on it is written again.
    ///
    /// # Errors
    ///
    /// Whatever the backend gives back when a write fails.
    pub fn reset_screen(&mut self) -> io::Result<()> {
        let screen = self.terminal.size()?;
        self.terminal.set_screen_size(screen);
        self.terminal.clear_screen()?;
        self.terminal
            .set_viewport_area(Rect::new(0, screen.height, screen.width, 0));
        Ok(())
    }

    /// Sizes the live area, writes any queued history, and draws.
    ///
    /// The order matters. History goes above the live area, so the live area
    /// has to be where it will be before anything is written above it;
    /// otherwise the rows land in the middle of a frame that is about to move.
    ///
    /// # Errors
    ///
    /// Whatever the backend gives back when a write fails.
    pub fn draw(&mut self, height: u16, render: impl FnOnce(&mut Frame)) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        let screen = self.terminal.size()?;
        let resized = screen != self.terminal.last_known_screen_size;
        self.terminal.set_screen_size(screen);

        let previous = self.terminal.viewport_area;
        let area = self.place(height, screen, resized)?;
        if area != previous {
            self.terminal.set_viewport_area(area);
        }

        if !self.pending.is_empty() {
            let lines = std::mem::take(&mut self.pending);
            self.reclaim =
                insert_history_lines(&mut self.terminal, &lines, self.mode, self.reclaim)?;
        }

        self.terminal.draw(render)
    }

    /// Where the live area goes, having made room for it.
    ///
    /// Four things can be true, and each needs a different repair:
    ///
    /// - **The window changed size.** Nothing this program believes about the
    ///   screen survives it, so the area is re-anchored at the bottom and
    ///   everything is painted again.
    /// - **The area grew past the bottom.** The rows it wants belong to the
    ///   history above it, so that history scrolls up to make room.
    /// - **The area shrank.** Its top stays put and it gives back rows at the
    ///   bottom, which still hold the old frame and are erased.
    ///
    /// **Shrinking gives back the bottom, not the top**, and that is the whole
    /// of how the live area behaves under a composer that is being typed into.
    /// A line arrives at the bottom of the box, so a line leaves from the
    /// bottom of it. Staying glued to the foot of the screen instead meant the
    /// *top* dropped when a line went, which pushed the box away from the
    /// conversation and opened a gap above it that nothing ever filled.
    ///
    /// Growth and shrink are then each other's opposite, which is what makes
    /// typing and untyping cost the conversation nothing: the rows given back
    /// at the bottom are the rows the next line grows back into, and no
    /// history has to scroll for either.
    fn place(&mut self, height: u16, screen: Size, resized: bool) -> io::Result<Rect> {
        let previous = self.terminal.viewport_area;
        if self.alt_screen {
            return Ok(Rect::new(0, 0, screen.width, screen.height));
        }

        let mut area = previous;
        area.width = screen.width;
        area.height = height.min(screen.height);

        if resized {
            area.y = screen.height.saturating_sub(area.height);
            // Nothing this program believed about the screen survives a
            // resize, including where it left a hole.
            self.reclaim = 0;
            // From whichever top is higher. A window that grew moves the live
            // area *down*, and the rows it leaves behind still hold the frame
            // it drew there: clearing only from the new top would leave a
            // second, stale copy of the composer above the real one.
            self.terminal.clear_after(Position {
                x: 0,
                y: previous.y.min(area.y),
            })?;
            return Ok(area);
        }

        if area.height < previous.height {
            // The top stays where it is. Erasing from it covers both the rows
            // given back at the bottom and whatever the old frame left in
            // them: without it a spinner row that came and went leaves a copy
            // of itself that nothing will ever draw over.
            self.terminal.clear_after(Position { x: 0, y: area.y })?;
            // The rows given back at the top are above the live area now, so
            // they are history with nothing in them. Remember how many, so the
            // next row out lands in the first of them.
            self.reclaim = self
                .reclaim
                .saturating_add(area.top().saturating_sub(previous.top()))
                .min(area.top());
            return Ok(area);
        }

        if area.bottom() > screen.height {
            let wanted = area.bottom() - screen.height;
            // Blank rows an earlier shrink gave back sit directly above the
            // live area, and growing back into them costs the history nothing.
            // Only what is left over is worth scrolling for.
            //
            // This is the difference between a composer that breathes and one
            // that walks up the screen. Shift-Return, a change of mind, and
            // Shift-Return again used to scroll the conversation up twice and
            // hand back a blank row in the middle — and a row that has gone
            // into the scrollback cannot be brought down again, so the drift
            // was permanent and accumulated for every line that came and went.
            let free = self.reclaim.min(wanted);
            self.scroll_history_up(area.top(), screen, wanted - free)?;
            area.y = screen.height - area.height;
        }
        // A top that moved up covers the hole rather than leaving it.
        self.reclaim = self
            .reclaim
            .saturating_sub(previous.top().saturating_sub(area.top()))
            .min(area.top());
        Ok(area)
    }

    /// Makes `by` rows of room above the live area by scrolling history up.
    fn scroll_history_up(&mut self, top: u16, screen: Size, by: u16) -> io::Result<()> {
        if by == 0 || top == 0 {
            return Ok(());
        }
        if top < 2 || self.mode == InsertMode::Repaint {
            // Too few rows for a scroll region, or a terminal that loses rows
            // scrolled out of one. Scroll the whole screen instead, which
            // costs the live area a repaint.
            self.terminal.clear_after(Position { x: 0, y: top })?;
            let writer = self.terminal.backend_mut();
            queue!(
                writer,
                crossterm::cursor::MoveTo(0, screen.height.saturating_sub(1))
            )?;
            for _ in 0..by {
                queue!(writer, crossterm::style::Print("\r\n"))?;
            }
            Write::flush(writer)?;
            self.terminal.invalidate();
            return Ok(());
        }

        let cursor = self.terminal.last_known_cursor_pos;
        let writer = self.terminal.backend_mut();
        queue!(
            writer,
            crate::insert_history::SetScrollRegion(1..top),
            crossterm::cursor::MoveTo(0, top - 1)
        )?;
        for _ in 0..by {
            queue!(writer, crossterm::style::Print("\r\n"))?;
        }
        queue!(
            writer,
            crate::insert_history::ResetScrollRegion,
            crossterm::cursor::MoveTo(cursor.x, cursor.y)
        )?;
        Write::flush(writer)
    }

    /// Takes the whole window, for a pager over the transcript.
    ///
    /// Queued history goes first: it belongs to the conversation the shell
    /// keeps, not to the screen that is about to be handed back.
    ///
    /// # Errors
    ///
    /// Whatever the backend gives back when a write fails.
    pub fn enter_alt_screen(&mut self) -> io::Result<()> {
        if self.alt_screen {
            return Ok(());
        }
        if !self.pending.is_empty() {
            let lines = std::mem::take(&mut self.pending);
            insert_history_lines(&mut self.terminal, &lines, self.mode, self.reclaim)?;
        }
        // The window is about to be borrowed whole, so a hole in it is gone.
        self.reclaim = 0;
        let screen = self.terminal.size()?;
        execute!(self.terminal.backend_mut(), EnterAlternateScreen)?;
        self.saved_viewport = Some(self.terminal.viewport_area);
        self.terminal
            .set_viewport_area(Rect::new(0, 0, screen.width, screen.height));
        self.terminal.clear()?;
        self.alt_screen = true;
        Ok(())
    }

    /// Gives the window back and puts the live area where it was.
    ///
    /// # Errors
    ///
    /// Whatever the backend gives back when a write fails.
    pub fn leave_alt_screen(&mut self) -> io::Result<()> {
        if !self.alt_screen {
            return Ok(());
        }
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
        if let Some(saved) = self.saved_viewport.take() {
            self.terminal.set_viewport_area(saved);
        }
        // The screen that came back holds what it held before the overlay, and
        // nothing this program drew since describes it.
        self.terminal.invalidate();
        self.alt_screen = false;
        self.alt_screen_leaves += 1;
        Ok(())
    }

    /// Whether the transcript has the window.
    #[must_use]
    pub fn is_alt_screen(&self) -> bool {
        self.alt_screen
    }

    /// How many times the window has been given back. See the field.
    #[must_use]
    pub fn alt_screen_leaves(&self) -> usize {
        self.alt_screen_leaves
    }

    /// Erases the live area and leaves the cursor on its first row.
    ///
    /// What follows is a shell prompt, and it belongs under the conversation
    /// rather than on top of the last frame of it. Idempotent: the ordinary
    /// return calls it and the drop calls it again.
    pub fn restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        if self.alt_screen {
            let _ = self.leave_alt_screen();
        }
        let top = self.terminal.viewport_area.y;
        let _ = self.terminal.clear();
        let _ = self.terminal.show_cursor();
        let _ = self.terminal.set_cursor_position(Position { x: 0, y: top });
        let _ = Write::flush(self.terminal.backend_mut());
        // Raw mode last, and only if this took it. A shell handed back with
        // raw mode still on is a shell with no echo, which reads as a hung
        // machine rather than as this program's fault.
        if self.owns_modes {
            restore_terminal();
        }
    }
}

impl<B> Drop for Tui<B>
where
    B: Backend<Error = io::Error> + Write,
{
    fn drop(&mut self) {
        self.restore();
    }
}

/// What a crossterm event means to the loop, if anything.
///
/// Releases and repeats of a key are not keystrokes twice; a mouse event is
/// something nothing asked for; a focus change is not this program's business.
#[must_use]
pub fn map_event(event: Event) -> Option<TuiEvent> {
    match event {
        Event::Key(key) => map_key(key).map(TuiEvent::Key),
        Event::Paste(text) => Some(TuiEvent::Paste(text)),
        Event::Resize(columns, rows) => Some(TuiEvent::Resize(Size {
            width: columns,
            height: rows,
        })),
        Event::FocusGained | Event::FocusLost | Event::Mouse(_) => None,
    }
}

/// A key event as a keystroke, dropping the ones that are not.
fn map_key(key: KeyEvent) -> Option<Key> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    Key::from_event(key)
}

/// Draw notifications as a stream, dropping the ones that lagged.
///
/// A lagged notification means more draws were asked for than were read, and
/// the answer to that is one draw of the current state.
///
/// Built with `unfold` rather than by polling `recv()` in a closure, and that
/// is not a style choice. A fresh `recv()` future dropped on a `Pending` poll
/// deregisters its waker, so the notification that arrives a frame later wakes
/// nothing: the draw is only noticed when something else happens to poll the
/// stream again. What that looks like is a character appearing on screen when
/// the *next* one is typed.
fn draw_stream(draws: broadcast::Receiver<()>) -> Pin<Box<dyn Stream<Item = TuiEvent> + Send>> {
    Box::pin(futures::stream::unfold(draws, |mut draws| async move {
        match draws.recv().await {
            Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => Some((TuiEvent::Draw, draws)),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }))
}

/// How this terminal wants its history written.
///
/// Windows Terminal drops rows scrolled out of a partial scroll region instead
/// of moving them into its scrollback, which would lose the conversation one
/// screenful at a time.
fn detect_insert_mode() -> InsertMode {
    if std::env::var_os("WT_SESSION").is_some() {
        InsertMode::Repaint
    } else {
        InsertMode::ScrollRegion
    }
}

/// Gives raw mode and bracketed paste back, whatever state they are in.
fn restore_terminal() {
    let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    let _ = execute!(io::stdout(), DisableBracketedPaste);
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), crossterm::cursor::Show);
}
