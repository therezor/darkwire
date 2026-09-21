//! What the prompt shows, and the one rule about where each row goes.
//!
//! A row is either *finished* or *live*. A finished row is written above the
//! live area, once, and belongs to the terminal from then on: its scrolling,
//! its selection, its search, its scrollback with no limit this program sets.
//! A live row is redrawn every frame and is never written anywhere — the
//! answer still arriving, the composer, the plan, the bar at the bottom.
//!
//! Everything else here follows from that. Folding is decided when a cell is
//! built rather than by a key pressed afterwards, because a row in the
//! scrollback cannot be taken back. Ctrl-T opens a transcript over the whole
//! window rather than unfolding in place. A long answer reaches the scrollback
//! while it is still being written, which is what lets the emulator's own
//! scroll be useful on the thing being read.
//!
//! The version this replaced held the whole exchange in the live area so that
//! a key could still fold it. What that cost: the live area grew with the
//! answer until it was most of the window, nothing reached the scrollback
//! until the turn ended, and scrolling back through a session showed a
//! conversation that had been re-drawn rather than one that had been printed.

use darkwire_protocol::config::{ReasoningDisplay, UiConfig};
use darkwire_tui::{
    Column, Editor, EditorOutcome, HistoryCell, Key, KeyName, Renderable, SelectItem, Theme,
    is_ctrl, spinner_frame, styled_line,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::bottom_pane::BottomPane;
use crate::header::HeaderView;
use crate::history_cell::{
    AssistantCell, DividerCell, FoldLabels, FoldedCell, NoticeCell, SessionHeaderCell,
    TurnStatsCell, UserCell,
};
use crate::pickers::palette::CommandChoice;
use crate::render::{LineKind, TranscriptEvent};
use crate::stream::StreamController;

/// How many rows the window has to have before a prompt is worth drawing.
const MIN_ROWS: usize = 6;

/// How long one spinner tick is, in milliseconds.
const SPINNER_INTERVAL_MS: i64 = darkwire_tui::SPINNER_INTERVAL_MS.cast_signed();

/// What a keystroke asked the prompt to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    /// A line was submitted.
    Line(String),
    /// Stop the running turn, or leave if none is.
    Interrupt,
    /// Leave.
    Leave,
    /// Open the palette.
    Palette,
    /// Complete the slash command being typed.
    Complete,
    /// Redraw and keep waiting.
    Redraw,
    /// Throw the screen away and draw it again.
    Reset,
    /// Show everything that has been said, over the whole window.
    Transcript,
    /// Change whether the *next* tool's output arrives expanded.
    ToggleTools,
    /// Change whether the *next* turn's cost arrives shown.
    ToggleStats,
}

/// Which kinds of run arrive folded away.
///
/// The defaults are both folded, and that is the argument the whole feature
/// rests on: a turn is read for its answer, and a terminal that prints every
/// line of the reasoning and every line of every tool result buries the one
/// thing the reader came for under the four things they can ask for.
#[derive(Debug, Clone, Copy)]
pub struct SummaryDefaults {
    /// How much of the model's reasoning arrives at all.
    pub reasoning: ReasoningDisplay,
    /// Whether a tool's output arrives folded.
    pub tools: bool,
    /// Whether what the turn cost arrives folded.
    pub stats: bool,
}

impl Default for SummaryDefaults {
    /// All three folded. Spelled out rather than derived, because `false` is
    /// the derived answer for the two switches and it is the wrong one.
    fn default() -> Self {
        Self {
            reasoning: ReasoningDisplay::Collapsed,
            tools: true,
            stats: true,
        }
    }
}

impl SummaryDefaults {
    /// What the install asked for.
    #[must_use]
    pub fn from_config(ui: &UiConfig) -> Self {
        Self {
            reasoning: ui.reasoning,
            tools: !ui.expand_tool_output,
            stats: !ui.expand_turn_stats,
        }
    }
}

/// What the live area is showing above the composer.
enum Active {
    /// An answer arriving.
    Assistant,
    /// A run of reasoning, with the tick it started on.
    Reasoning {
        /// The tick it started on, for a run being watched.
        since: i64,
        /// What it took, for a run read back from storage.
        elapsed_ms: Option<u64>,
    },
    /// A tool's output, under the row that says how the call went.
    Tool { summary: String },
}

/// The conversation, the composer, and the rule about where a row goes.
pub struct ChatWidget {
    theme: Theme,
    labels: FoldLabels,
    /// The word beside the spinner while a turn is thinking.
    generating: String,
    stream: StreamController,
    active: Option<Active>,
    /// Every cell, in order, for the transcript. Nothing is ever removed
    /// except when the prompt moves to another conversation.
    committed: Vec<Box<dyn HistoryCell>>,
    /// Rows written but not yet handed to the terminal.
    pending: Vec<Line<'static>>,
    defaults: SummaryDefaults,
    /// What a key last did to `/output stats`, waiting to be collected.
    stats_toggled: Option<bool>,
    /// The tick the spinner is on, while a turn is thinking.
    thinking: Option<i64>,
    /// Whether the events arriving are a conversation that already happened.
    ///
    /// A replayed run of reasoning has no duration: the store keeps what was
    /// said, not how long the saying took, and a summary counting from zero
    /// would put `0ms` on a turn that thought for a minute last week.
    replaying: bool,
    running: bool,
    bottom: BottomPane,
    /// How wide and tall the window is.
    width: u16,
    height: u16,
}

impl ChatWidget {
    /// A prompt with nothing said in it yet.
    #[must_use]
    pub fn new(theme: Theme, labels: FoldLabels, generating: &str) -> Self {
        let editor = Editor::new(&theme);
        let bottom = BottomPane::new(editor, HeaderView::default(), theme);
        Self {
            theme,
            labels,
            generating: generating.to_owned(),
            stream: StreamController::new(),
            active: None,
            committed: Vec::new(),
            pending: Vec::new(),
            defaults: SummaryDefaults::default(),
            stats_toggled: None,
            thinking: None,
            replaying: false,
            running: false,
            bottom,
            width: 0,
            height: 0,
        }
    }

    /// Which kinds of run arrive folded.
    pub fn set_defaults(&mut self, defaults: SummaryDefaults) {
        self.defaults = defaults;
    }

    /// The commands the list offers.
    pub fn set_commands(&mut self, commands: Vec<SelectItem<CommandChoice>>) {
        self.bottom.set_commands(commands);
    }

    /// Tells the widget how big the window is.
    pub fn set_screen_size(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        self.bottom.set_window_rows(usize::from(height));
    }

    /// The pane at the bottom, for the loop that pushes views onto it.
    pub fn bottom_mut(&mut self) -> &mut BottomPane {
        &mut self.bottom
    }

    /// Replaces what the bar at the bottom says.
    pub fn set_view(&mut self, view: HeaderView) {
        self.bottom.set_view(view);
    }

    /// The rows the session opens with.
    pub fn open_with_header(&mut self, header: &str) {
        self.commit(Box::new(SessionHeaderCell::new(header)));
    }

    /// What is on the composer line right now.
    #[must_use]
    pub fn typing(&self) -> &str {
        self.bottom.typing()
    }

    /// Holds a line until the turn in front of it finishes.
    pub fn queue(&mut self, line: String) {
        self.bottom.queue(line);
    }

    /// The next line waiting, taken.
    pub fn take_queued(&mut self) -> Option<String> {
        self.bottom.take_queued()
    }

    /// Whether the cost of a turn reaches a reader.
    #[must_use]
    pub fn stats_shown(&self) -> bool {
        !self.defaults.stats
    }

    /// What a key last did to that switch, answered once.
    ///
    /// One switch with two owners. A key sets it here, because a key is how
    /// the reader asks; the renderer owns it the rest of the time, because
    /// only the renderer decides whether a pipe sees the row at all.
    pub fn take_stats_toggle(&mut self) -> Option<bool> {
        self.stats_toggled.take()
    }

    // ------------------------------------------------------------ the turn

    /// A turn has started.
    pub fn start_turn(&mut self) {
        self.running = true;
        self.thinking = Some(0);
    }

    /// A turn has finished, however it finished.
    ///
    /// Whatever was arriving is committed as what it managed to say: an answer
    /// interrupted mid-sentence still said what it said, and leaving it in the
    /// live area would lose it at the next frame.
    pub fn end_turn(&mut self) {
        self.finish_active();
        self.running = false;
        self.thinking = None;
        self.bottom.set_spinner(None);
    }

    /// Whether anything on screen is moving.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.thinking.is_some()
    }

    /// One spinner tick. `true` when something moved.
    ///
    /// The count is the turn's clock as well as the spinner's frame, and it
    /// runs from the first tick to the last whatever is on screen meanwhile. A
    /// clock stopped by the first word of an answer reports `0ms` for every run
    /// of reasoning after it, which is what it used to do.
    pub fn tick(&mut self) -> bool {
        let Some(tick) = self.thinking else {
            return false;
        };
        self.thinking = Some(tick + 1);
        // A run of reasoning spins on its own summary row, and text arriving is
        // its own sign of life. The bar spins when neither is true: the turn is
        // running and nothing is on its way yet, which is where the wait
        // between a tool result and the answer to it lives.
        if self.active.is_none() && self.stream.is_empty() {
            self.bottom
                .set_spinner(Some((tick + 1, self.generating.clone())));
        } else {
            self.bottom.set_spinner(None);
        }
        true
    }

    /// The operator's own message, which opens an exchange.
    pub fn echo(&mut self, content: &str) {
        self.finish_active();
        self.commit(Box::new(UserCell::new(content, &self.theme)));
    }

    /// One thing the turn said.
    ///
    /// Every event carries its own kind, so what a cell becomes is a property
    /// of what arrived rather than a guess about where a write landed. A pipe
    /// reads the same events and renders them flat.
    pub fn handle_event(&mut self, event: &TranscriptEvent) {
        match event {
            TranscriptEvent::AssistantDelta { text, depth } => {
                if self.active.is_none() {
                    self.active = Some(Active::Assistant);
                }
                self.stream.push(text, *depth);
                self.clear_spinner_unless_reasoning();
            }
            TranscriptEvent::ReasoningDelta { text, depth } => {
                self.stream.push(text, *depth);
            }
            TranscriptEvent::EndLine => self.stream.end_line(),
            TranscriptEvent::ReasoningStart { elapsed_ms } => {
                self.finish_active();
                self.active = Some(Active::Reasoning {
                    since: self.thinking.unwrap_or(0),
                    elapsed_ms: *elapsed_ms,
                });
                // The run has a spinner of its own on its summary row. Two
                // spinners a few rows apart, turning out of step, read as two
                // things happening.
                self.bottom.set_spinner(None);
            }
            TranscriptEvent::ReasoningEnd => {
                let (since, elapsed_ms) = match self.active {
                    Some(Active::Reasoning { since, elapsed_ms }) => (since, elapsed_ms),
                    _ => (0, None),
                };
                let summary = self.summary_for(since, elapsed_ms, true);
                let body = self.stream.take_all();
                let expanded = self.defaults.reasoning == ReasoningDisplay::Expanded;
                self.active = None;
                if !body.is_empty() {
                    self.commit(Box::new(FoldedCell::new(
                        styled_line(&summary),
                        body,
                        expanded,
                    )));
                }
            }
            TranscriptEvent::ToolBodyStart { summary } => {
                self.finish_active();
                self.active = Some(Active::Tool {
                    summary: summary.trim_end_matches('\n').to_owned(),
                });
            }
            TranscriptEvent::ToolBodyEnd => {
                let summary = match self.active.take() {
                    Some(Active::Tool { summary }) => summary,
                    // A body that ended without having started is a renderer
                    // bug, not something to lose the output over.
                    other => {
                        self.active = other;
                        String::new()
                    }
                };
                let body = self.stream.take_all();
                let expanded = !self.defaults.tools;
                self.commit(Box::new(FoldedCell::new(
                    styled_line(&summary),
                    body,
                    expanded,
                )));
            }
            TranscriptEvent::Line { kind, text } => {
                if *kind == LineKind::Echo {
                    self.echo(text);
                    return;
                }
                // A tool's own output arrives as complete lines inside the run
                // it belongs to, so it joins that body rather than becoming a
                // cell of its own beside it.
                if matches!(self.active, Some(Active::Tool { .. })) {
                    self.stream.push_line(text);
                } else {
                    self.finish_active();
                    self.commit(Box::new(NoticeCell::new(text)));
                }
                self.clear_spinner_unless_reasoning();
            }
            TranscriptEvent::Tasks(tasks) => self.bottom.set_tasks(tasks),
            // The plan lives above the composer, so the card never reaches
            // here: `ChunkSink` drops it at the source.
            TranscriptEvent::TasksCard { .. } => {}
            TranscriptEvent::TurnStats { line, .. } => {
                self.finish_active();
                self.commit(Box::new(TurnStatsCell::new(line, !self.defaults.stats)));
            }
            // `/output reasoning off` and `ui.reasoning: hidden` are the same
            // state reached two ways.
            TranscriptEvent::ReasoningShown(shown) => {
                self.defaults.reasoning = if *shown {
                    ReasoningDisplay::Collapsed
                } else {
                    ReasoningDisplay::Hidden
                };
            }
            // Set rather than flipped: this is the command's half of one
            // switch arriving, and a flip here would undo what was asked for.
            TranscriptEvent::StatsShown(shown) => self.defaults.stats = !*shown,
        }
    }

    /// Whatever was arriving, committed as what it managed to say.
    fn finish_active(&mut self) {
        match self.active.take() {
            None => {
                // A stream with no owner is an answer nobody opened a cell
                // for, which happens on a replay.
                let lines = self.stream.take_all();
                if !lines.is_empty() {
                    self.commit(Box::new(AssistantCell::new(lines)));
                }
            }
            Some(Active::Assistant) => {
                let lines = self.stream.take_all();
                if !lines.is_empty() {
                    self.commit(Box::new(AssistantCell::new(lines)));
                }
            }
            Some(Active::Reasoning { since, elapsed_ms }) => {
                let summary = self.summary_for(since, elapsed_ms, true);
                let body = self.stream.take_all();
                let expanded = self.defaults.reasoning == ReasoningDisplay::Expanded;
                if !body.is_empty() {
                    self.commit(Box::new(FoldedCell::new(
                        styled_line(&summary),
                        body,
                        expanded,
                    )));
                }
            }
            Some(Active::Tool { summary }) => {
                let body = self.stream.take_all();
                let expanded = !self.defaults.tools;
                self.commit(Box::new(FoldedCell::new(
                    styled_line(&summary),
                    body,
                    expanded,
                )));
            }
        }
    }

    /// The spinner stood in for an answer that had not started.
    ///
    /// A fold opening is not an answer starting, which is why this is a call
    /// and not something done on every event: a collapsed run of reasoning
    /// would otherwise clear the one thing on screen that was moving.
    ///
    /// The turn's clock is left running. It is not the spinner, even though
    /// they count in the same units.
    fn clear_spinner_unless_reasoning(&mut self) {
        if !matches!(self.active, Some(Active::Reasoning { .. })) {
            self.bottom.set_spinner(None);
        }
    }

    /// Writes a cell: its rows queue for the terminal, the cell itself stays
    /// for the transcript.
    fn commit(&mut self, cell: Box<dyn HistoryCell>) {
        let width = self.width.max(1);
        self.pending.extend(cell.display_lines(width));
        self.committed.push(cell);
    }

    /// The rows waiting to go to the terminal, taken.
    pub fn drain_history(&mut self) -> Vec<Line<'static>> {
        std::mem::take(&mut self.pending)
    }

    /// Every cell, for the transcript.
    #[must_use]
    pub fn cells(&self) -> &[Box<dyn HistoryCell>] {
        &self.committed
    }

    /// The last `rows` rows of the conversation, at `width`.
    ///
    /// What a window that changed size has to be given again. The rows above
    /// the live area were written at the old width and are the terminal's now;
    /// a terminal that reflows them puts them where the new width says, which
    /// is not where this program left them. Erasing the window and writing the
    /// tail again is what makes the screen say what it said before.
    #[must_use]
    pub fn history_tail(&self, width: u16, rows: usize) -> Vec<Line<'static>> {
        let mut tail: Vec<Line<'static>> = Vec::new();
        for cell in self.committed.iter().rev() {
            let mut lines = cell.display_lines(width);
            lines.extend(std::mem::take(&mut tail));
            tail = lines;
            if tail.len() >= rows {
                break;
            }
        }
        if tail.len() > rows {
            tail.drain(..tail.len() - rows);
        }
        tail
    }

    /// The row a run of reasoning shows.
    ///
    /// The figure is appended rather than interpolated into a sentence, so the
    /// translation is a word and the duration is a duration. A template with a
    /// count in it would need plural rules to say "1 second" in the languages
    /// that have them, for a row that is read at a glance.
    fn reasoning_summary(&self, done: bool) -> String {
        let (since, elapsed_ms) = match self.active {
            Some(Active::Reasoning { since, elapsed_ms }) => (since, elapsed_ms),
            _ => (0, None),
        };
        self.summary_for(since, elapsed_ms, done)
    }

    /// The same row, for a run whose start is known without reading `active`.
    ///
    /// Finishing a run takes it out of `active` first, and a summary built
    /// after that would count from zero: an interrupted run would report the
    /// whole turn as its thinking time.
    fn summary_for(&self, since: i64, elapsed_ms: Option<u64>, done: bool) -> String {
        let (mark, word) = if done {
            ("┄".to_owned(), &self.labels.reasoning)
        } else {
            (
                spinner_frame(self.thinking.unwrap_or(0)).to_owned(),
                &self.labels.thinking,
            )
        };
        // Three cases, and the middle one is why this is not simply the figure
        // or simply the clock. A run being watched is timed by the ticks this
        // has counted. A stored run brings its own figure. A run stored before
        // the figure was kept has neither, and says nothing rather than
        // reporting a clock that started when the prompt opened.
        let elapsed = match (elapsed_ms, self.replaying) {
            (Some(stored), _) => Some(crate::render::to_ms(stored)),
            (None, true) => None,
            (None, false) => {
                let ticks = self.thinking.unwrap_or(since).saturating_sub(since).max(0);
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "a tick count large enough to lose precision is a turn lasting weeks"
                )]
                let measured = (ticks as f64) * (SPINNER_INTERVAL_MS as f64);
                Some(measured)
            }
        };
        let row = match elapsed {
            None => format!("{mark} {word}"),
            Some(elapsed) => format!("{mark} {word} {}", crate::render::format_duration(elapsed)),
        };
        self.theme.dim.apply(&row)
    }

    // ------------------------------------------------------------- history

    /// Everything a session already said, written as it was.
    ///
    /// The events are the ones a live turn emits, built from the stored
    /// messages by the same renderer, so a conversation somebody came back to
    /// reads the way it read while it was running: the reasoning folded to a
    /// row, the calls badged and folded under what they answered. Anything a
    /// cell would have taken from the clock is left off, because the clock was
    /// running last week.
    pub fn replay(&mut self, history: &[TranscriptEvent]) {
        self.replaying = true;
        for event in history {
            self.handle_event(event);
        }
        // `EndLine` settles an open line; it does not commit one. A
        // conversation whose last message was an answer would otherwise leave
        // that answer in the stream rather than in the scrollback.
        self.finish_active();
        self.replaying = false;
    }

    /// The prompt has moved to another conversation.
    ///
    /// Nothing is erased, because nothing can be: what is above belongs to the
    /// terminal now. A rule says where one conversation ended, and the
    /// transcript starts again so Ctrl-T shows the session being read rather
    /// than two of them run together.
    pub fn reopen(&mut self, header: &str, history: &[TranscriptEvent]) {
        self.finish_active();
        self.commit(Box::new(DividerCell::new(
            usize::from(self.width.max(1)),
            &self.theme,
        )));
        self.committed.clear();
        self.bottom.clear_tasks();
        self.commit(Box::new(SessionHeaderCell::new(header)));
        self.replay(history);
    }

    // ----------------------------------------------------------- the keys

    /// What a keystroke asked the prompt to do.
    ///
    /// The order is the rule. Whatever is stacked over the composer sees a key
    /// first and usually takes it; then the command list, which takes only the
    /// keys that steer it and lets the rest through; then the chords; then the
    /// composer.
    pub fn handle_key(&mut self, key: &Key) -> Typed {
        if self.bottom.has_view() {
            self.bottom.offer_key(key);
            return Typed::Redraw;
        }

        if self.bottom.has_popup()
            && let Some(typed) = self.popup_key(key)
        {
            return typed;
        }

        if is_ctrl(key, 'g') {
            return Typed::Palette;
        }
        if is_ctrl(key, 't') {
            return Typed::Transcript;
        }
        if is_ctrl(key, 'o') {
            self.defaults.tools = !self.defaults.tools;
            return Typed::ToggleTools;
        }
        if is_ctrl(key, 'y') {
            self.defaults.stats = !self.defaults.stats;
            self.stats_toggled = Some(!self.defaults.stats);
            return Typed::ToggleStats;
        }
        if is_ctrl(key, 'l') {
            return Typed::Reset;
        }
        if key.name == KeyName::Tab && !self.bottom.has_popup() {
            return Typed::Complete;
        }

        let outcome = self.bottom.editor_mut().handle_key(key);
        self.bottom.sync_popup();
        match outcome {
            EditorOutcome::Submit(line) => {
                self.bottom.close_popup();
                Typed::Line(line)
            }
            EditorOutcome::Interrupt => Typed::Interrupt,
            EditorOutcome::Eof => Typed::Leave,
            EditorOutcome::None => Typed::Redraw,
        }
    }

    /// The keys that steer the command list, when one is open.
    ///
    /// `None` for a key the list wants nothing to do with, which then reaches
    /// the composer: a list that swallowed every key would be one you could
    /// not type a filter into.
    fn popup_key(&mut self, key: &Key) -> Option<Typed> {
        // A key that means "a new line in what I am writing" belongs to the
        // composer whatever is open beside it.
        if darkwire_tui::is_newline(key) {
            return None;
        }
        match key.name {
            KeyName::Up => {
                self.bottom.move_popup(-1);
                Some(Typed::Redraw)
            }
            KeyName::Down => {
                self.bottom.move_popup(1);
                Some(Typed::Redraw)
            }
            KeyName::Escape => {
                self.bottom.close_popup();
                Some(Typed::Redraw)
            }
            KeyName::Tab => Some(self.accept_popup(false)),
            KeyName::Enter => Some(self.accept_popup(true)),
            _ => None,
        }
    }

    /// Puts the highlighted command on the composer line, or runs it.
    ///
    /// Tab completes and Return runs, but only a command that takes no
    /// argument runs on Return: completing `/model` and submitting it are the
    /// same keystroke otherwise, and one of them is a mistake.
    fn accept_popup(&mut self, run: bool) -> Typed {
        let Some(choice) = self.bottom.popup_choice() else {
            return Typed::Redraw;
        };
        self.bottom.close_popup();
        if run && choice.submit {
            self.bottom.editor_mut().set_text("");
            self.bottom.editor_mut().remember(&choice.command);
            return Typed::Line(choice.command);
        }
        self.bottom
            .editor_mut()
            .set_text(&format!("{} ", choice.command));
        Typed::Redraw
    }

    // ---------------------------------------------------------- the layout

    /// How many rows the live area may give the answer still arriving.
    ///
    /// Half the window. The other half is the composer, the chrome and, above
    /// all, the conversation the terminal is holding: a live area that filled
    /// the window would be the alternate screen with extra steps.
    fn active_cap(&self) -> usize {
        (usize::from(self.height) / 2).max(1)
    }

    /// The rows of whatever is arriving, and what of it has gone already.
    ///
    /// A long answer is written to the scrollback as it goes rather than in
    /// one lump at the end: once the live area is at its cap, the oldest
    /// settled lines are finished text that will not change, so they go.
    fn live_rows(&mut self) -> Vec<Line<'static>> {
        let width = self.width.max(1);
        let cap = self.active_cap();
        let mut rows = self.stream.rows(width);

        // Only an answer flushes early. A collapsed run shows one summary row
        // however long it gets, so it never reaches the cap; an expanded one
        // is the same finished text an answer is.
        let flushes = matches!(self.active, Some(Active::Assistant) | None)
            || matches!(self.active, Some(Active::Reasoning { .. }))
                && self.defaults.reasoning == ReasoningDisplay::Expanded
            || matches!(self.active, Some(Active::Tool { .. })) && !self.defaults.tools;

        if rows.len() > cap && flushes {
            let keep = cap.saturating_sub(1).max(1);
            let taken = self.stream.take_settled(keep);
            self.pending.extend(taken);
            rows = self.stream.rows(width);
        }

        // Whatever is still too tall is shown from the bottom: the newest rows
        // are the ones being read.
        if rows.len() > cap {
            rows.drain(..rows.len() - cap);
        }
        rows
    }

    /// The summary row an open run shows while it is still open.
    fn open_summary(&self) -> Option<Line<'static>> {
        match &self.active {
            Some(Active::Reasoning { .. }) => Some(styled_line(&self.reasoning_summary(false))),
            Some(Active::Tool { summary }) => Some(styled_line(summary)),
            Some(Active::Assistant) | None => None,
        }
    }

    /// Everything the live area draws, top to bottom.
    ///
    /// One description of the layout, so the height the terminal is asked for
    /// and the rows drawn into it cannot disagree. They did once, and what
    /// that produces is a composer clipped off the bottom of its own area.
    fn column(&mut self) -> Column<'static> {
        let mut column = Column::new();
        if self.too_small() {
            column.push(vec![styled_line(
                &self.theme.dim.apply(&self.labels.too_small),
            )]);
            return column;
        }

        let mut above: Vec<Line<'static>> = Vec::new();
        if let Some(summary) = self.open_summary() {
            above.push(summary);
        }
        // A collapsed run shows its summary and nothing else: its body is
        // going to the transcript, not to the screen.
        let showing_body = !matches!(self.active, Some(Active::Reasoning { .. }))
            || self.defaults.reasoning == ReasoningDisplay::Expanded;
        if showing_body {
            above.extend(self.live_rows());
        }
        if !above.is_empty() {
            above.push(Line::default());
            column.push(above);
        }
        column.push(self.bottom.render_rows(usize::from(self.width.max(1))));
        column
    }

    /// Whether there is any point drawing a prompt at all.
    fn too_small(&self) -> bool {
        usize::from(self.height) < MIN_ROWS || usize::from(self.width) < crate::menu::MIN_COLUMNS
    }

    /// How tall the live area has to be.
    pub fn desired_height(&mut self, width: u16) -> u16 {
        if self.too_small() {
            return 1;
        }
        let height = self.column().desired_height(width);
        // Never the whole window: what the conversation is holding above the
        // live area has to stay visible.
        height.clamp(1, self.height.saturating_sub(1).max(1))
    }

    /// Draws the live area.
    pub fn render(&mut self, area: Rect, buffer: &mut Buffer) {
        self.column().render(area, buffer);
    }

    /// Where the caret belongs, if this owns it.
    pub fn cursor_pos(&mut self, area: Rect) -> Option<(u16, u16)> {
        self.column().cursor_pos(area)
    }
}
