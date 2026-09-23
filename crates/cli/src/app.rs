//! The prompt's one loop, and the only thing that owns the terminal.
//!
//! Everything the prompt has to react to arrives here: text from the model,
//! keystrokes, the window changing size, a request to draw, a picker a slash
//! command opened, the spinner, and the turn finishing. One `select!` covers
//! all of it, one `&mut self` owns all of it, and nothing is behind a lock.
//!
//! That is the change worth naming. The version this replaced had four copies
//! of the same key-and-draw loop — one for the idle prompt, one for a running
//! turn, one for an open menu, one for an open listing — because a picker had
//! to keep reading the keyboard while the code that opened it was awaiting an
//! answer. They shared a `FrameState` behind a mutex and drifted: a key
//! handled in three of them, a resize in two.
//!
//! What makes one loop possible is [`Surface::attend`]. A slash command that
//! opens a picker is handed *back* to the loop to drive, so the picker is a
//! view on the bottom pane and the answer comes back down a channel. The
//! command still writes `let chosen = menu.choose(..).await`, and nothing in
//! it knows there is a loop.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use darkwire_core::Result;
use darkwire_i18n::keys;
use darkwire_tui::tui::{StdoutTui, Tui, TuiEvent};
use darkwire_tui::{
    Pages, PagesOptions, PagesOutcome, Renderable, Select, SelectOptions, SelectOutcome, Theme,
    frame::FrameRequester,
};
use futures::StreamExt;
use ratatui::layout::Size;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::ask_overlay::{AskOutcome, AskOverlay};
use crate::bottom_pane::SelectView;
use crate::chat::{BoxFut, ChatSession, ChunkSink, Flow, Surface, TurnOutcome, chunks};
use crate::chat_widget::{ChatWidget, SummaryDefaults, Typed};
use crate::header::{HeaderView, startup_header};
use crate::history_cell::FoldLabels;
use crate::i18n::Translations;
use crate::pickers::palette::{PaletteRow, complete_command};
use crate::pickers::{
    AskRequest, ListingRequest, MenuAnswer, MenuRequest as PickerRequest, PickerMenu, Placement,
};
use crate::render::TranscriptEvent;
use crate::select_overlay::SelectOverlay;
use crate::transcript_overlay::{OverlayOutcome, TranscriptOverlay};

/// How long one spinner tick is.
const SPINNER_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(darkwire_tui::SPINNER_INTERVAL_MS);

/// Something only the loop can do, asked for from somewhere else.
enum AppEvent {
    /// Put a menu on the bottom pane and answer when it closes.
    Menu(PickerRequest, oneshot::Sender<Option<MenuAnswer>>),
    /// Put a listing on the bottom pane and say when it closes.
    Listing(ListingRequest, oneshot::Sender<()>),
    /// Put a question on the window and answer with the line typed into it.
    Ask(AskRequest, oneshot::Sender<Option<String>>),
}

/// A picker, as a channel to the loop.
///
/// `Send + Sync` by construction rather than by locking: the request goes down
/// a channel and the answer comes back up one, so a command holding this does
/// not hold anything the loop needs.
struct AppMenu {
    events: mpsc::UnboundedSender<AppEvent>,
}

impl PickerMenu for AppMenu {
    fn available(&self) -> bool {
        true
    }

    fn choose<'a>(
        &'a self,
        request: PickerRequest,
    ) -> Pin<Box<dyn Future<Output = Option<MenuAnswer>> + Send + 'a>> {
        Box::pin(async move {
            let (answer, chosen) = oneshot::channel();
            if self.events.send(AppEvent::Menu(request, answer)).is_err() {
                return None;
            }
            // A loop that has gone answers nothing, which is the same as a
            // cancelled menu: the command carries on and asks for nothing.
            chosen.await.unwrap_or(None)
        })
    }

    fn ask<'a>(
        &'a self,
        request: AskRequest,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> {
        Box::pin(async move {
            let (answer, typed) = oneshot::channel();
            if self.events.send(AppEvent::Ask(request, answer)).is_err() {
                return None;
            }
            typed.await.unwrap_or(None)
        })
    }

    fn show<'a>(
        &'a self,
        request: ListingRequest,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            let (answer, closed) = oneshot::channel();
            if self
                .events
                .send(AppEvent::Listing(request, answer))
                .is_err()
            {
                return false;
            }
            closed.await.is_ok()
        })
    }
}

/// Something drawn over the whole window instead of the prompt.
///
/// Every one of these is opened, worked in, and closed: they scroll or they
/// are typed into, and they take nothing from the conversation while they are
/// open. That is what the alternate screen is for, and it is the only thing
/// this program uses it for — the prompt itself never takes it, because a
/// prompt that did would own scrolling and selection and would take the
/// session with it on exit.
enum Overlay {
    /// Everything that has been said, with nothing folded away.
    Transcript(TranscriptOverlay),
    /// A listing opened to be read, such as `/help`.
    ///
    /// Boxed because a set of tabbed pages is much larger than a transcript,
    /// and this is held in the prompt whether or not one is open.
    Listing(Box<Pages>, Option<oneshot::Sender<()>>),
    /// A list too long to pick from five rows at a time, such as the sessions.
    Menu(
        Box<SelectOverlay>,
        Option<oneshot::Sender<Option<MenuAnswer>>>,
    ),
    /// A question with a line to answer it on, such as a workspace's new name.
    Ask(Box<AskOverlay>, Option<oneshot::Sender<Option<String>>>),
}

impl Overlay {
    /// One keystroke, against a window `height` rows tall.
    fn handle_key(&mut self, key: &darkwire_tui::Key, height: u16) -> OverlayOutcome {
        match self {
            Overlay::Transcript(transcript) => transcript.handle_key(key, height),
            Overlay::Listing(pages, answer) => {
                if pages.handle_key(key) == PagesOutcome::Closed {
                    if let Some(answer) = answer.take() {
                        let _ = answer.send(());
                    }
                    OverlayOutcome::Closed
                } else {
                    OverlayOutcome::Open
                }
            }
            Overlay::Ask(ask, answer) => {
                let typed = match ask.handle_key(key) {
                    AskOutcome::Open => return OverlayOutcome::Open,
                    AskOutcome::Answered(line) => Some(line),
                    AskOutcome::Cancelled => None,
                };
                if let Some(answer) = answer.take() {
                    let _ = answer.send(typed);
                }
                OverlayOutcome::Closed
            }
            Overlay::Menu(menu, answer) => {
                let _ = height;
                let chosen = match menu.handle_key(key) {
                    SelectOutcome::Open => return OverlayOutcome::Open,
                    SelectOutcome::Chosen(row) => Some(MenuAnswer { row, action: None }),
                    SelectOutcome::Acted { action, value } => Some(MenuAnswer {
                        row: value,
                        action: Some(action),
                    }),
                    SelectOutcome::Cancelled => None,
                };
                if let Some(answer) = answer.take() {
                    let _ = answer.send(chosen);
                }
                OverlayOutcome::Closed
            }
        }
    }

    /// Where the caret belongs, for the one overlay that is typed into.
    fn cursor_pos(&mut self, area: ratatui::layout::Rect) -> Option<(u16, u16)> {
        match self {
            Overlay::Transcript(_) | Overlay::Listing(..) => None,
            Overlay::Menu(menu, _) => menu.cursor_pos(area),
            Overlay::Ask(ask, _) => ask.cursor_pos(area),
        }
    }

    /// Draws it over the window.
    fn render(&mut self, area: ratatui::layout::Rect, buffer: &mut ratatui::buffer::Buffer) {
        match self {
            Overlay::Transcript(transcript) => {
                darkwire_tui::Renderable::render(transcript, area, buffer);
            }
            Overlay::Listing(pages, _) => {
                let rows = darkwire_tui::Component::render(pages.as_mut(), usize::from(area.width));
                darkwire_tui::StyledRows::new(&rows).render(area, buffer);
            }
            Overlay::Menu(menu, _) => menu.render(area, buffer),
            Overlay::Ask(ask, _) => ask.render(area, buffer),
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // A listing dropped without being closed still has a caller waiting
        // on it, and a caller waiting for ever is a prompt that has stopped.
        match self {
            Overlay::Listing(_, answer) => {
                if let Some(answer) = answer.take() {
                    let _ = answer.send(());
                }
            }
            Overlay::Menu(_, answer) => {
                if let Some(answer) = answer.take() {
                    let _ = answer.send(None);
                }
            }
            Overlay::Ask(_, answer) => {
                if let Some(answer) = answer.take() {
                    let _ = answer.send(None);
                }
            }
            Overlay::Transcript(_) => {}
        }
    }
}

/// What the loop was asked to drive alongside the prompt.
enum Job<'a> {
    /// A turn, which an interrupt cancels.
    Turn {
        token: &'a CancellationToken,
        body: BoxFut<'a, Result<TurnOutcome>>,
    },
    /// A slash command, which may open a picker.
    Command(BoxFut<'a, Flow>),
}

/// What the loop stopped for.
enum Pumped {
    /// A line was submitted.
    Line(String),
    /// Leave.
    Leave,
    /// The turn finished.
    Turn(Result<TurnOutcome>),
    /// The command finished.
    Command(Flow),
}

/// A future that is only ready when there is one.
///
/// The arm of a `select!` that has nothing to drive has to be unreachable
/// rather than panicking or busy-waiting, and a future that never resolves is
/// exactly that: the arm is polled, stays pending, and nothing else changes.
async fn when<T>(slot: Option<&mut (impl Future<Output = T> + Unpin)>) -> T {
    match slot {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}

/// The prompt on a terminal.
///
/// Generic over the backend for the same reason [`Tui`] is: everything with a
/// decision in it — what a key does, when a line is queued rather than
/// returned, what reaches the scrollback and when — is then drivable over a
/// terminal emulator in memory. [`TuiSurface::open`] is the only thing that
/// needs a real one.
pub struct TuiSurface<B = ratatui::backend::CrosstermBackend<std::io::Stdout>>
where
    B: ratatui::backend::Backend<Error = std::io::Error> + std::io::Write,
{
    tui: Tui<B>,
    requester: FrameRequester,
    events: Pin<Box<dyn futures::Stream<Item = TuiEvent> + Send>>,
    widget: ChatWidget,
    overlay: Option<Overlay>,
    sink: ChunkSink,
    chunks: mpsc::UnboundedReceiver<TranscriptEvent>,
    menu: Arc<dyn PickerMenu>,
    app_rx: mpsc::UnboundedReceiver<AppEvent>,
    rows: Vec<PaletteRow>,
    /// What the bar at the bottom says, for a session that is replaced.
    view: HeaderView,
    /// The palette's answer, while one is open.
    ///
    /// The palette is a picker like any other. What is different is that
    /// nothing is awaiting it — the loop is, which is what this holds.
    palette: Option<oneshot::Receiver<Option<MenuAnswer>>>,
    /// What the rows of the open palette mean.
    palette_items: Vec<darkwire_tui::SelectItem<crate::pickers::palette::CommandChoice>>,
    t: Translations,
    theme: Theme,
    overlay_footer: String,
    closed: bool,
}

impl<B> std::fmt::Debug for TuiSurface<B>
where
    B: ratatui::backend::Backend<Error = std::io::Error> + std::io::Write,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TuiSurface")
    }
}

impl TuiSurface<ratatui::backend::CrosstermBackend<std::io::Stdout>> {
    /// Takes the terminal and draws the session as it was left.
    ///
    /// # Errors
    ///
    /// When raw mode or the terminal's size cannot be had, in which case the
    /// caller falls back to the plain prompt.
    pub fn open(session: &ChatSession) -> Result<Self> {
        let tui = StdoutTui::init().map_err(|error| {
            darkwire_core::WireError::new(
                darkwire_core::ErrorKind::Internal,
                format!("the terminal refused raw mode: {error}"),
            )
        })?;
        let width = tui.size().map_or(80, |size| usize::from(size.width));
        let events = tui.event_stream();

        let mut surface = Self::over(tui, session);
        surface.listen_to(events);
        // A resumed session has a plan and a conversation already. Reading
        // them here rather than waiting for the agent means the first thing on
        // screen is where the work had got to: a prompt that opened on twenty
        // exchanges and showed a blank screen made every resumed session look
        // like a new one.
        let header = startup_header(&session.view(), width, &session.theme, &session.t, true);
        surface.restore_session(session, &header);
        Ok(surface)
    }
}

impl<B> TuiSurface<B>
where
    B: ratatui::backend::Backend<Error = std::io::Error> + std::io::Write,
{
    /// The prompt over a terminal that is already open.
    ///
    /// What [`TuiSurface::open`] ends with, and what a test starts with.
    pub fn over(tui: Tui<B>, session: &ChatSession) -> Self {
        let size = tui.size().unwrap_or(Size {
            width: 80,
            height: 24,
        });
        let mut widget = ChatWidget::new(
            session.theme,
            FoldLabels {
                thinking: session.t.t(keys::chat::folds::THINKING),
                reasoning: session.t.t(keys::chat::folds::REASONING),
                too_small: session.t.t(keys::chat::TOO_SMALL),
            },
            &session.t.t(keys::chat::GENERATING),
        );
        widget.set_screen_size(size.width, size.height);
        widget.set_defaults(SummaryDefaults::from_config(&session.runtime.config().ui));
        widget.set_view(session.view());
        widget.set_commands(crate::pickers::palette::command_items(
            &crate::commands::palette_rows(&session.runtime),
            &session.t,
        ));

        let (app_tx, app_rx) = mpsc::unbounded_channel();
        let (sink, chunks) = chunks();
        let requester = tui.frame_requester();
        let menu: Arc<dyn PickerMenu> = Arc::new(AppMenu { events: app_tx });
        // From here on there is somebody to ask.
        if let Some(gate) = &session.approvals {
            gate.attend(Arc::clone(&menu));
        }
        Self {
            tui,
            requester,
            events: Box::pin(futures::stream::empty()),
            widget,
            overlay: None,
            sink,
            chunks,
            menu,
            app_rx,
            rows: crate::commands::palette_rows(&session.runtime),
            view: session.view(),
            palette: None,
            palette_items: Vec::new(),
            t: Translations::new(session.t.locale()),
            theme: session.theme,
            overlay_footer: session.t.t(keys::chat::TRANSCRIPT_FOOTER),
            closed: false,
        }
    }

    /// Where the things this reacts to come from.
    ///
    /// A test supplies a scripted one; [`TuiSurface::open`] supplies the
    /// terminal's, merged with the draw notifications.
    pub fn listen_to(&mut self, events: Pin<Box<dyn futures::Stream<Item = TuiEvent> + Send>>) {
        self.events = events;
    }

    /// The conversation as it was left, and the plan it had got to.
    pub fn restore_session(&mut self, session: &ChatSession, header: &str) {
        if let Ok(stored) = session
            .runtime
            .store()
            .tasks(&session.attachment.session_key)
        {
            self.widget.bottom_mut().set_tasks(
                &stored
                    .iter()
                    .map(|task| (task.status, task.text.clone()))
                    .collect::<Vec<_>>(),
            );
        }
        self.widget.open_with_header(header);
        self.widget.replay(&crate::chat::replayed(session));
        // Drawn now rather than left to the first event. The loop only draws
        // when something wakes it, and nothing has yet: the header and the
        // conversation would sit in the queue until a key was pressed, and
        // what an operator would see in the meantime is an empty screen.
        self.draw();
    }

    /// What the prompt is showing, for a test that has to read it.
    #[must_use]
    pub fn widget_mut(&mut self) -> &mut ChatWidget {
        &mut self.widget
    }

    /// The terminal under the prompt, for a test that has to read the screen.
    #[must_use]
    pub fn tui(&self) -> &Tui<B> {
        &self.tui
    }

    /// One turn of the loop, with whatever it was asked to drive alongside.
    async fn pump(&mut self, job: Option<Job<'_>>) -> Pumped {
        let mut spins = tokio::time::interval(SPINNER_INTERVAL);
        let idle = job.is_none();
        let (token, mut driving): (Option<&CancellationToken>, Option<BoxFut<'_, Pumped>>) =
            match job {
                Some(Job::Turn { token, body }) => (
                    Some(token),
                    Some(Box::pin(async move { Pumped::Turn(body.await) })),
                ),
                Some(Job::Command(body)) => (
                    None,
                    Some(Box::pin(async move { Pumped::Command(body.await) })),
                ),
                None => (None, None),
            };
        // A provider fast enough to keep the chunk arm permanently ready would
        // starve every arm below it, and the screen would freeze for the
        // length of the answer. Yielding after a run of them is what lets the
        // others come round.
        let mut since_yield = 0usize;

        loop {
            let stopped = tokio::select! {
                biased;
                Some(event) = self.chunks.recv() => {
                    self.widget.handle_event(&event);
                    self.requester.schedule_frame();
                    since_yield += 1;
                    if since_yield >= 64 {
                        since_yield = 0;
                        tokio::task::yield_now().await;
                    }
                    None
                }
                Some(event) = self.events.next() => {
                    since_yield = 0;
                    self.handle_tui_event(event, token, idle)
                }
                Some(event) = self.app_rx.recv() => {
                    self.open_view(event);
                    None
                }
                _ = spins.tick(), if self.widget.is_animating() => {
                    if self.widget.tick() {
                        self.requester.schedule_frame();
                    }
                    None
                }
                chosen = when(self.palette.as_mut()) => {
                    self.palette = None;
                    self.take_palette_choice(chosen.ok().flatten(), idle)
                }
                done = when(driving.as_mut()) => Some(done),
            };

            if let Some(stopped) = stopped {
                // Anything the turn said after the last frame, before the
                // caller is told it ended.
                while let Ok(event) = self.chunks.try_recv() {
                    self.widget.handle_event(&event);
                }
                if matches!(stopped, Pumped::Turn(_)) {
                    self.widget.end_turn();
                }
                self.draw();
                return stopped;
            }
        }
    }

    /// One thing the terminal said.
    fn handle_tui_event(
        &mut self,
        event: TuiEvent,
        token: Option<&CancellationToken>,
        idle: bool,
    ) -> Option<Pumped> {
        match event {
            TuiEvent::Draw => self.draw(),
            TuiEvent::Resize(size) => {
                self.widget.set_screen_size(size.width, size.height);
                self.rebuild_overlay();
                self.rebuild_screen(size);
                self.requester.schedule_frame();
            }
            TuiEvent::Paste(text) => {
                // Bracketed paste arrives whole; the composer is a single line
                // so a newline in it is a space rather than a submission
                // nobody asked for.
                let text = text.replace(['\r', '\n'], " ");
                for character in text.chars() {
                    self.widget.handle_key(&darkwire_tui::Key::char(character));
                }
                self.requester.schedule_frame();
            }
            TuiEvent::Key(key) => return self.handle_key(&key, token, idle),
        }
        None
    }

    /// One keystroke.
    fn handle_key(
        &mut self,
        key: &darkwire_tui::Key,
        token: Option<&CancellationToken>,
        idle: bool,
    ) -> Option<Pumped> {
        if let Some(overlay) = self.overlay.as_mut() {
            let height = self.tui.size().map_or(24, |size| size.height);
            if overlay.handle_key(key, height) == OverlayOutcome::Closed {
                // The screen is given back in `draw`, not here. A menu with
                // verbs on its rows closes so its caller can apply one and
                // opens again a moment later; leaving the second buffer in
                // between would flash the conversation up and take it away.
                self.overlay = None;
            }
            self.requester.schedule_frame();
            return None;
        }

        let typed = self.widget.handle_key(key);
        self.requester.schedule_frame();
        match typed {
            Typed::Line(line) => {
                // A line submitted while anything else is running waits for
                // it. Nothing is dropped, which is what lets somebody write
                // the next question while the current answer is streaming —
                // and what keeps a line typed during a slash command from
                // being handed back to a caller that cannot take one.
                if !idle {
                    self.widget.queue(line);
                    return None;
                }
                return Some(Pumped::Line(line));
            }
            // While a turn runs an interrupt belongs to the turn; at an idle
            // prompt it means "leave", which is what the shell's own meant.
            Typed::Interrupt => match token {
                Some(token) => token.cancel(),
                None => return Some(Pumped::Leave),
            },
            // Leaving is refused while anything runs: the answer is still
            // being written into the conversation this would tear down.
            Typed::Leave if idle => return Some(Pumped::Leave),
            Typed::Transcript => self.open_transcript(),
            Typed::Reset => {
                self.tui.invalidate();
            }
            Typed::Complete => self.complete(),
            Typed::Palette => self.open_palette(),
            Typed::Leave | Typed::Redraw | Typed::ToggleTools | Typed::ToggleStats => {}
        }
        None
    }

    /// Puts a picker on the bottom pane.
    fn open_view(&mut self, event: AppEvent) {
        match event {
            AppEvent::Menu(request, answer) => {
                let window = self.tui.size().unwrap_or(Size {
                    width: 80,
                    height: 24,
                });
                let rows = match request.placement {
                    Placement::Prompt => self.widget.bottom_mut().max_menu_rows(),
                    Placement::Window => usize::from(window.height),
                };
                let select = Select::new(SelectOptions {
                    items: request.items,
                    labels: request.labels,
                    theme: Some(self.theme),
                    index: request.index,
                    max_rows: Some(rows),
                    actions: request.actions,
                });
                match request.placement {
                    Placement::Prompt => self
                        .widget
                        .bottom_mut()
                        .push_view(Box::new(SelectView::new(select, answer))),
                    // A list as long as the install is old, filtered by typing.
                    // Five rows of it under the composer is a filter applied
                    // blind, so it takes the window a listing takes.
                    Placement::Window => self.open_overlay(Overlay::Menu(
                        Box::new(SelectOverlay::new(select, window.height)),
                        Some(answer),
                    )),
                }
            }
            // A listing takes the window rather than a slot at the foot of
            // it. It is a document: opened to be read, scrolled through, and
            // closed. Laying one over the prompt instead would mean growing
            // the live area by twenty rows, and the live area grows by
            // scrolling the conversation into the terminal's scrollback,
            // which closing it again cannot undo.
            AppEvent::Ask(request, answer) => {
                let ask = AskOverlay::new(
                    &request.title,
                    &request.initial,
                    &self.t.t(keys::slash::ask::FOOTER),
                    &self.theme,
                );
                self.open_overlay(Overlay::Ask(Box::new(ask), Some(answer)));
            }
            AppEvent::Listing(request, answer) => {
                let size = self.tui.size().unwrap_or(Size {
                    width: 80,
                    height: 24,
                });
                let pages = Pages::new(PagesOptions {
                    pages: request.pages,
                    labels: request.labels,
                    theme: Some(self.theme),
                    max_rows: Some(usize::from(size.height).saturating_sub(1).max(1)),
                });
                self.open_overlay(Overlay::Listing(Box::new(pages), Some(answer)));
            }
        }
        self.requester.schedule_frame();
    }

    /// Ctrl-T: everything that was said, over the whole window.
    fn open_transcript(&mut self) {
        let width = self.tui.size().map_or(80, |size| size.width);
        self.open_overlay(Overlay::Transcript(TranscriptOverlay::new(
            self.widget.cells(),
            width,
            &self.theme,
            &self.overlay_footer,
        )));
    }

    /// Puts a document over the window.
    fn open_overlay(&mut self, overlay: Overlay) {
        self.overlay = Some(overlay);
        // The queued rows go out before the window is taken: they belong to
        // the conversation the shell keeps, not to the screen being borrowed.
        let pending = self.widget.drain_history();
        self.tui.insert_history_lines(pending);
        let _ = self.tui.enter_alt_screen();
        self.requester.schedule_frame();
    }

    /// Draws the window again for a size it has not been drawn at.
    ///
    /// The rows above the live area were written at the old width and belong
    /// to the terminal. An emulator that reflows them on a resize puts them
    /// where the new width says, which is not where this program left them:
    /// what is left behind is a copy of the live area for every step of a
    /// drag, and nothing here can address those rows to erase them.
    ///
    /// So the window is erased and the tail of the conversation written again
    /// at the new width. What had already scrolled out of the window is in the
    /// terminal's scrollback and is not touched.
    fn rebuild_screen(&mut self, size: Size) {
        // Nothing to rebuild while the window is not this program's ordinary
        // screen. The second condition is the moment between a menu closing on
        // a verb and opening again: no overlay, but the alternate screen is
        // still held, and writing the conversation into it would put a copy of
        // it under the menu that is about to be drawn.
        if self.overlay.is_some() || self.tui.is_alt_screen() {
            return;
        }
        if self.tui.reset_screen().is_err() {
            return;
        }
        let live = self.widget.desired_height(size.width);
        let room = usize::from(size.height.saturating_sub(live));
        let tail = self.widget.history_tail(size.width, room);
        self.tui.insert_history_lines(tail);
    }

    /// Builds the transcript again for a window that changed size.
    fn rebuild_overlay(&mut self) {
        let size = self.tui.size().unwrap_or(Size {
            width: 80,
            height: 24,
        });
        match self.overlay.as_mut() {
            // A list is rebuilt by telling it how many rows it now has. It
            // folds nothing, so nothing has to be folded again.
            Some(Overlay::Menu(menu, _)) => menu.resize(size.height),
            Some(Overlay::Transcript(open)) if open.width() != size.width => {
                self.overlay = Some(Overlay::Transcript(TranscriptOverlay::new(
                    self.widget.cells(),
                    size.width,
                    &self.theme,
                    &self.overlay_footer,
                )));
            }
            // A listing folds its own rows on the way in and is redrawn from
            // them, and a question is one line and a caret. Neither needs
            // anything here.
            Some(Overlay::Transcript(_) | Overlay::Listing(..) | Overlay::Ask(..)) | None => {}
        }
    }

    /// Tab: the one command that completes what is typed, or nothing.
    fn complete(&mut self) {
        let (matches, _) = complete_command(self.widget.typing(), &self.rows);
        if let [only] = matches.as_slice() {
            let text = format!("{only} ");
            self.widget.bottom_mut().editor_mut().set_text(&text);
            self.widget.bottom_mut().sync_popup();
        }
        self.requester.schedule_frame();
    }

    /// Ctrl-G: every command as one searchable list.
    ///
    /// Built here rather than through [`pick_command`], which would have to be
    /// awaited and cannot be: the translator holds an `Rc`, so the future is
    /// not `Send` and nothing can spawn it. The list is the same list.
    fn open_palette(&mut self) {
        let items = crate::pickers::palette::command_items(&self.rows, &self.t);
        let (answer, chosen) = oneshot::channel();
        let request = PickerRequest {
            items: items
                .iter()
                .enumerate()
                .map(|(at, item)| darkwire_tui::SelectItem {
                    value: at,
                    label: item.label.clone(),
                    hint: item.hint.clone(),
                    keywords: item.keywords.clone(),
                    disabled: item.disabled,
                })
                .collect(),
            labels: crate::pickers::labels(&self.t.t(keys::menu::titles::COMMAND), &self.t),
            index: None,
            placement: Placement::Prompt,
            actions: Vec::new(),
        };
        self.palette_items = items;
        self.open_view(AppEvent::Menu(request, answer));
        self.palette = Some(chosen);
    }

    /// What the palette was pointing at when it closed.
    ///
    /// A command that needs an argument lands on the composer line with the
    /// caret after it. One that needs nothing is queued rather than submitted:
    /// the loop is what submits, and it is in the middle of a turn of its own.
    fn take_palette_choice(&mut self, at: Option<MenuAnswer>, idle: bool) -> Option<Pumped> {
        let choice = at
            .and_then(|at| self.palette_items.get(at.row))
            .map(|item| item.value.clone());
        self.palette_items = Vec::new();
        self.requester.schedule_frame();
        let choice = choice?;
        if !choice.submit {
            let text = format!("{} ", choice.command);
            self.widget.bottom_mut().editor_mut().set_text(&text);
            self.widget.bottom_mut().sync_popup();
            return None;
        }
        // A command that needs nothing runs, which at an idle prompt means
        // handing it back the way a typed line is. While something else is
        // running it waits, the same as anything else typed meanwhile.
        if idle {
            return Some(Pumped::Line(choice.command));
        }
        self.widget.queue(choice.command);
        None
    }

    /// Writes what is finished and draws what is not.
    fn draw(&mut self) {
        if self.closed {
            return;
        }
        if let Some(overlay) = self.overlay.as_mut() {
            let height = self.tui.size().map_or(24, |size| size.height);
            let _ = self.tui.draw(height, |frame| {
                let area = frame.area;
                overlay.render(area, frame.buffer);
                if let Some((x, y)) = overlay.cursor_pos(area) {
                    frame.set_cursor_position((x, y));
                }
            });
            return;
        }
        // Nothing is over the prompt any more, so the second buffer goes back.
        // Deferred to here rather than done when the overlay closed: a close
        // and a re-open inside one frame then never leaves it at all.
        if self.tui.is_alt_screen() {
            let _ = self.tui.leave_alt_screen();
        }
        // The height first. Working it out is also what notices that the
        // live area is over its cap and flushes the oldest settled rows, so
        // draining before it would leave those rows in neither place until
        // the next frame.
        let width = self.tui.size().map_or(80, |size| size.width);
        let height = self.widget.desired_height(width);
        let pending = self.widget.drain_history();
        self.tui.insert_history_lines(pending);

        let widget = &mut self.widget;
        let _ = self.tui.draw(height, |frame| {
            let area = frame.area;
            widget.render(area, frame.buffer);
            if let Some((x, y)) = widget.cursor_pos(area) {
                frame.set_cursor_position((x, y));
            }
        });
    }
}

impl<B> Surface for TuiSurface<B>
where
    B: ratatui::backend::Backend<Error = std::io::Error> + std::io::Write,
{
    fn menu(&self) -> Arc<dyn PickerMenu> {
        Arc::clone(&self.menu)
    }

    fn sink(&self) -> ChunkSink {
        self.sink.clone()
    }

    fn next_line(&mut self) -> BoxFut<'_, Option<String>> {
        Box::pin(async move {
            // Anything a previous turn queued goes first, without waiting for
            // another keystroke: a message sent while an answer was streaming
            // is a message that was already asked for.
            if let Some(held) = self.widget.take_queued() {
                self.draw();
                return Some(held);
            }
            match self.pump(None).await {
                Pumped::Line(line) => Some(line),
                // Nothing else can stop a pump with no job to drive.
                Pumped::Leave | Pumped::Turn(_) | Pumped::Command(_) => None,
            }
        })
    }

    fn run<'a>(
        &'a mut self,
        token: &'a CancellationToken,
        body: BoxFut<'a, Result<TurnOutcome>>,
    ) -> BoxFut<'a, Result<TurnOutcome>> {
        Box::pin(async move {
            // Nothing to point at while a turn runs: the caret would otherwise
            // sit on a composer whose screen is still being written above it,
            // and read as a prompt that is ready.
            self.widget.start_turn();
            self.draw();
            match self.pump(Some(Job::Turn { token, body })).await {
                Pumped::Turn(outcome) => outcome,
                // The turn arm is the only one that can stop a turn's pump.
                Pumped::Line(_) | Pumped::Leave | Pumped::Command(_) => {
                    Err(darkwire_core::WireError::new(
                        darkwire_core::ErrorKind::Internal,
                        "the prompt stopped without the turn finishing",
                    ))
                }
            }
        })
    }

    fn attend<'a>(&'a mut self, body: BoxFut<'a, Flow>) -> BoxFut<'a, Flow> {
        Box::pin(async move {
            match self.pump(Some(Job::Command(body))).await {
                Pumped::Command(flow) => flow,
                // The command arm is the only one that can stop its pump.
                Pumped::Line(_) | Pumped::Leave | Pumped::Turn(_) => Flow::Again,
            }
        })
    }

    fn echo<'a>(&'a mut self, content: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move {
            self.widget.echo(content);
            self.draw();
        })
    }

    fn refresh<'a>(&'a mut self, view: &'a HeaderView) -> BoxFut<'a, ()> {
        Box::pin(async move {
            while let Ok(event) = self.chunks.try_recv() {
                self.widget.handle_event(&event);
            }
            self.view = view.clone();
            self.widget.set_view(view.clone());
            self.draw();
        })
    }

    fn compose<'a>(&'a mut self, text: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move {
            // The same door the command palette uses for a command that needs
            // an argument: the line, a space, and the caret after it.
            self.widget.bottom_mut().editor_mut().set_text(text);
            self.draw();
        })
    }

    fn transcript(&mut self) -> BoxFut<'_, bool> {
        Box::pin(async move {
            self.open_transcript();
            true
        })
    }

    fn reopen<'a>(
        &'a mut self,
        view: &'a HeaderView,
        history: &'a [TranscriptEvent],
    ) -> BoxFut<'a, ()> {
        Box::pin(async move {
            // Anything the command itself said goes first: the note saying
            // which conversation this now is belongs to the screen it names,
            // not to the one being replaced.
            while let Ok(event) = self.chunks.try_recv() {
                self.widget.handle_event(&event);
            }
            let width = self.tui.size().map_or(80, |size| size.width);
            let header = startup_header(view, usize::from(width), &self.theme, &self.t, true);
            self.view = view.clone();
            self.widget.set_view(view.clone());
            self.widget.reopen(&header, history);
            self.draw();
        })
    }

    fn take_stats_shown(&mut self) -> BoxFut<'_, Option<bool>> {
        Box::pin(std::future::ready(self.widget.take_stats_toggle()))
    }

    fn close(&mut self) -> BoxFut<'_, ()> {
        Box::pin(async move {
            if self.closed {
                return;
            }
            // Whatever is still queued goes to the scrollback rather than
            // disappearing with the live area.
            let pending = self.widget.drain_history();
            self.tui.insert_history_lines(pending);
            let _ = self.tui.draw(1, |_| {});
            self.closed = true;
            // Done here rather than left to the drop, because the runtime
            // closing behind this prints to the terminal underneath.
            self.tui.restore();
        })
    }
}
