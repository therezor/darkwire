//! The live rows at the bottom of the screen, and what stacks over them.
//!
//! Everything here is *now*: what is being typed, what is waiting, what the
//! plan is, what the bar at the bottom says. None of it is history and none of
//! it goes to the scrollback. That is the line this module draws — a row that
//! belongs here is one that will be different a second from now, and a row
//! that will not is a [`crate::history_cell`].
//!
//! Top to bottom: the spinner, the plan, the queue, the rule, the composer,
//! whatever is stacked over the composer, and the status bar. A view goes
//! *under* the composer rather than over it. A list that hid the line it was
//! filtering would be a list you had to close to see what you had asked for,
//! and a menu that replaced the composer would take the caret with it.

use std::collections::VecDeque;

use darkwire_protocol::TaskStatus;
use darkwire_tui::{
    CHROME_ROWS as SELECT_CHROME_ROWS, Editor, Key, Select, SelectItem, SelectList, SelectOutcome,
    StyledRows, Theme, cursor_in, spinner_frame, truncate_to_width,
};

use crate::header::{HeaderView, input_rule, status_bar};
use crate::pickers::MenuAnswer;
use crate::pickers::palette::CommandChoice;

/// How many rows of the plan are shown above the composer.
const TASK_ROWS: usize = 3;

/// How many queued messages are shown above the composer.
const QUEUED_ROWS: usize = 3;

/// How many commands the list under the composer shows at once.
///
/// Fixed, so the list is the same size whatever is typed into it: a list that
/// grew and shrank with the filter would move the composer under the fingers
/// of whoever is filtering it. Fewer on a window too short to spare them.
const POPUP_ROWS: usize = 5;

/// How many rows the slot leaves the rest of the pane on a short window.
///
/// The rule and the composer are two of them; the other three are slack for
/// whatever is above those, which varies. Not [`SELECT_CHROME_ROWS`], which is
/// a menu's own chrome and a different quantity that happened to be the same
/// number: shrinking that one to match the new chrome would grow the slot here
/// and clip the composer off the bottom.
const RESERVED_ROWS: usize = 5;

/// What a key did to a view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKey {
    /// The view took it.
    Consumed,
    /// The view wants nothing to do with it.
    PassThrough,
}

/// Something stacked over the composer for as long as it has a question.
pub trait BottomPaneView {
    /// What this did with a keystroke.
    fn handle_key(&mut self, key: &Key) -> ViewKey;

    /// Whether this has its answer and should be taken away.
    fn is_complete(&self) -> bool;

    /// The rows this wants at `width`.
    fn rows(&mut self, width: usize) -> Vec<String>;

    /// The line this wants drawn where the composer would be, if it wants one.
    ///
    /// A view is stacked over the composer while it has a question, and the
    /// composer is not being typed into meanwhile. A menu with a filter has a
    /// question *and* something being typed, so it says so here and gets the
    /// row rather than leaving an empty prompt above itself.
    fn prompt(&mut self, width: usize) -> Option<String> {
        let _ = width;
        None
    }
}

/// A menu, waiting for a row to be chosen.
///
/// The answer goes back down the channel the picker is waiting on. Sending it
/// from inside the key handler is what makes a cancelled menu and a chosen one
/// the same shape: both are an answer, one of them is `None`.
pub struct SelectView {
    select: Select<usize>,
    answer: Option<tokio::sync::oneshot::Sender<Option<MenuAnswer>>>,
}

impl SelectView {
    /// A menu that will answer down this channel.
    #[must_use]
    pub fn new(
        select: Select<usize>,
        answer: tokio::sync::oneshot::Sender<Option<MenuAnswer>>,
    ) -> Self {
        Self {
            select,
            answer: Some(answer),
        }
    }

    fn answer(&mut self, chosen: Option<MenuAnswer>) {
        if let Some(answer) = self.answer.take() {
            let _ = answer.send(chosen);
        }
    }
}

impl BottomPaneView for SelectView {
    fn handle_key(&mut self, key: &Key) -> ViewKey {
        match self.select.handle_key(key) {
            SelectOutcome::Open => {}
            SelectOutcome::Chosen(row) => self.answer(Some(MenuAnswer { row, action: None })),
            SelectOutcome::Acted { action, value } => self.answer(Some(MenuAnswer {
                row: value,
                action: Some(action),
            })),
            SelectOutcome::Cancelled => self.answer(None),
        }
        ViewKey::Consumed
    }

    fn is_complete(&self) -> bool {
        self.answer.is_none()
    }

    fn rows(&mut self, width: usize) -> Vec<String> {
        darkwire_tui::Component::render(&mut self.select, width)
    }

    fn prompt(&mut self, width: usize) -> Option<String> {
        Some(truncate_to_width(&self.select.prompt(), width, "…"))
    }
}

/// The composer and everything drawn around it.
pub struct BottomPane {
    editor: Editor,
    views: Vec<Box<dyn BottomPaneView>>,
    /// The commands the list offers.
    commands: Vec<SelectItem<CommandChoice>>,
    /// The list open under the composer, while a slash is being typed.
    ///
    /// Not a view. A view takes every key, and this one has to let most of
    /// them through: it is a filter on what is being typed, so typing has to
    /// keep reaching the composer while it is open.
    popup: Option<SelectList<CommandChoice>>,
    queued: VecDeque<String>,
    tasks: Vec<(TaskStatus, String)>,
    /// The spinner's tick and the word beside it, while a turn is running.
    spinner: Option<(i64, String)>,
    view: HeaderView,
    theme: Theme,
    /// How tall the window is, which is what caps a long message and a menu.
    window_rows: usize,
}

impl BottomPane {
    /// An empty composer and nothing over it.
    #[must_use]
    pub fn new(editor: Editor, view: HeaderView, theme: Theme) -> Self {
        Self {
            editor,
            views: Vec::new(),
            commands: Vec::new(),
            popup: None,
            queued: VecDeque::new(),
            tasks: Vec::new(),
            spinner: None,
            view,
            theme,
            window_rows: 0,
        }
    }

    /// The composer, for the key map and for completion.
    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// What is on the composer line right now.
    #[must_use]
    pub fn typing(&self) -> &str {
        self.editor.text()
    }

    /// Replaces what the bar at the bottom says.
    pub fn set_view(&mut self, view: HeaderView) {
        self.view = view;
    }

    /// The commands the list offers.
    pub fn set_commands(&mut self, commands: Vec<SelectItem<CommandChoice>>) {
        self.commands = commands;
    }

    /// Whether a command list is open under the composer.
    #[must_use]
    pub fn has_popup(&self) -> bool {
        self.popup.is_some()
    }

    /// Closes the command list.
    pub fn close_popup(&mut self) {
        self.popup = None;
    }

    /// Moves the cursor in the open command list.
    pub fn move_popup(&mut self, delta: i64) {
        if let Some(popup) = self.popup.as_mut() {
            popup.move_by(delta);
        }
    }

    /// The command the list is pointing at.
    #[must_use]
    pub fn popup_choice(&self) -> Option<CommandChoice> {
        self.popup
            .as_ref()
            .and_then(SelectList::selected)
            .map(|item| item.value.clone())
    }

    /// How many commands the list shows.
    ///
    /// A window with no rows to spare gets a shorter list rather than a
    /// composer pushed off the bottom of it.
    #[must_use]
    pub fn popup_rows(&self) -> usize {
        POPUP_ROWS
            .min(self.window_rows.saturating_sub(RESERVED_ROWS))
            .max(1)
    }

    /// Opens, filters or closes the list from what is being typed.
    ///
    /// A slash on its own opens it; a space closes it, because the command has
    /// been chosen and what follows is its argument.
    pub fn sync_popup(&mut self) {
        let text = self.editor.text().to_owned();
        if !text.starts_with('/') || text.contains(' ') {
            self.popup = None;
            return;
        }
        let rows = self.popup_rows();
        if let Some(popup) = self.popup.as_mut() {
            popup.set_rows(rows);
            popup.set_filter(&text);
        } else {
            let mut popup = SelectList::new(self.commands.clone(), Some(rows), None);
            popup.set_filter(&text);
            self.popup = Some(popup);
        }
    }

    /// Replaces the plan shown above the composer.
    ///
    /// Whole, because that is what the `todo` tool does: there is no add and no
    /// complete, so the list that arrived is the list.
    pub fn set_tasks(&mut self, tasks: &[(TaskStatus, String)]) {
        self.tasks = tasks.to_vec();
    }

    /// Throws the plan away.
    pub fn clear_tasks(&mut self) {
        self.tasks.clear();
    }

    /// Shows or hides the spinner and the word beside it.
    pub fn set_spinner(&mut self, spinner: Option<(i64, String)>) {
        self.spinner = spinner;
    }

    /// Tells the pane how big the window is.
    pub fn set_window_rows(&mut self, rows: usize) {
        self.window_rows = rows;
    }

    /// Holds a line until the turn in front of it finishes.
    pub fn queue(&mut self, line: String) {
        self.queued.push_back(line);
    }

    /// The next line waiting, taken.
    pub fn take_queued(&mut self) -> Option<String> {
        self.queued.pop_front()
    }

    /// Puts something over the composer.
    pub fn push_view(&mut self, view: Box<dyn BottomPaneView>) {
        self.views.push(view);
    }

    /// Whether anything is stacked over the composer.
    #[must_use]
    pub fn has_view(&self) -> bool {
        !self.views.is_empty()
    }

    /// Offers a keystroke to whatever is stacked over the composer.
    ///
    /// The top view sees it first and usually takes it. A view that answered
    /// its question is taken away here rather than at the next draw, so the
    /// key that closed it does not also reach the composer underneath.
    pub fn offer_key(&mut self, key: &Key) -> ViewKey {
        let Some(view) = self.views.last_mut() else {
            return ViewKey::PassThrough;
        };
        let taken = view.handle_key(key);
        if view.is_complete() {
            self.views.pop();
        }
        taken
    }

    /// How many rows a menu stacked here may take.
    ///
    /// The slot it will be drawn in, and no more. A menu that asked for more
    /// would make the live area taller, and the live area grows by scrolling
    /// the conversation into the terminal's scrollback — which closing the
    /// menu again cannot undo. A menu with more rows than that scrolls.
    #[must_use]
    pub fn max_menu_rows(&self) -> usize {
        self.slot_rows().saturating_sub(SELECT_CHROME_ROWS).max(1)
    }

    /// Every row of the pane, top to bottom.
    ///
    /// One description of the layout, so the height the live area is sized to
    /// and the rows drawn into it cannot disagree. They did once, and what
    /// that produces is a composer clipped off the bottom of its own area.
    ///
    /// `editor_rows` caps how much of a long message is shown. `None` is the
    /// honest answer when there is no window to fit it into.
    fn rows(&mut self, width: usize, editor_rows: Option<usize>) -> Vec<String> {
        let mut rows = Vec::new();
        if let Some((tick, word)) = self.spinner.clone() {
            rows.push(
                self.theme
                    .dim
                    .apply(&format!("{} {word}", spinner_frame(tick))),
            );
        }
        rows.extend(self.task_rows(width));
        rows.extend(self.queued_rows(width));
        rows.push(input_rule(width, &self.theme));
        match self.views.last_mut().and_then(|view| view.prompt(width)) {
            // The menu's question and its filter, on the row the composer would
            // have had. The composer is not being typed into while a view holds
            // the keys, so the row is the menu's to use.
            Some(prompt) => rows.push(prompt),
            None => rows.extend(around_cursor(
                darkwire_tui::Component::render(&mut self.editor, width),
                editor_rows,
            )),
        }
        rows.extend(self.slot(width));
        rows
    }

    /// The rows below the composer, whatever is using them.
    ///
    /// One slot, always the same height, holding either the bar at the bottom
    /// or a list being picked from. Two things follow from that, and both are
    /// the point:
    ///
    /// - **A list takes the bar's rows rather than pushing them down.** The
    ///   bar says which model and which workspace, which is worth a glance
    ///   between questions and nothing at all while a list is open.
    /// - **Opening one costs the conversation nothing.** The live area is at
    ///   the foot of the screen, so growing it scrolls what is above into the
    ///   scrollback — and shrinking it again cannot bring those rows back. A
    ///   slot that is one height forever never grows, so the conversation is
    ///   where it was when the list closes.
    ///
    /// A list goes *under* the composer, never over it: one that hid the line
    /// it was filtering would be a list you had to close to read what you had
    /// asked for.
    fn slot(&mut self, width: usize) -> Vec<String> {
        let height = self.slot_rows();
        let mut rows = if let Some(view) = self.views.last_mut() {
            let mut rows = view.rows(width);
            // Whatever it asked for, it gets the slot: a menu that made the
            // live area taller would cost the conversation rows it cannot
            // give back.
            rows.truncate(height);
            rows
        } else if let Some(popup) = self.popup.as_ref() {
            let mut rows = popup.render(width, &self.theme);
            if let Some((at, total)) = popup.counter() {
                let counter = format!("  ({at}/{total})");
                rows.push(
                    self.theme
                        .dim
                        .apply(&truncate_to_width(&counter, width, "…")),
                );
            }
            rows
        } else {
            status_bar(&self.view, width, &self.theme)
        };
        // The spare rows go at the foot, under everything, rather than
        // between the composer and the bar. A gap that opened in the middle
        // every time a list closed would read as something missing; one at the
        // bottom of the screen reads as the bottom of the screen.
        while rows.len() < height {
            rows.push(String::new());
        }
        rows
    }

    /// How many rows the slot below the composer takes.
    ///
    /// One height, always: the bar, a command list and a menu all live in it,
    /// and none of them changes the live area by arriving.
    fn slot_rows(&self) -> usize {
        self.popup_rows() + 1
    }

    /// The pane's rows, as something a frame can draw.
    ///
    /// Not a [`darkwire_tui::Renderable`] impl, because rendering needs `&mut`
    /// all the way down: the composer caches the wrap it last produced. The
    /// widget above calls this and puts the answer in its column.
    pub fn render_rows(&mut self, width: usize) -> StyledRows {
        let cap = (self.window_rows / 3).max(1);
        StyledRows::new(&self.rows(width, Some(cap)))
    }

    /// How tall the pane is at this width.
    pub fn desired_height(&mut self, width: u16) -> u16 {
        let cap = (self.window_rows / 3).max(1);
        let rows = self.rows(usize::from(width), Some(cap)).len();
        u16::try_from(rows).unwrap_or(u16::MAX)
    }

    /// The plan, as a window around whatever is in hand.
    ///
    /// Capped, because the question this answers is "where has this got to",
    /// and a ten-row list above the box you type into answers it worse than
    /// three rows do. The window is centred on the task in progress.
    fn task_rows(&self, width: usize) -> Vec<String> {
        if self.tasks.is_empty() {
            return Vec::new();
        }
        let doing = self
            .tasks
            .iter()
            .position(|(status, _)| *status == TaskStatus::Doing)
            .unwrap_or(0);
        let first = doing
            .saturating_sub(1)
            .min(self.tasks.len().saturating_sub(TASK_ROWS));
        let mut rows: Vec<String> = self
            .tasks
            .iter()
            .skip(first)
            .take(TASK_ROWS)
            .map(|(status, text)| {
                truncate_to_width(&task_row(&self.theme, *status, text), width, "…")
            })
            .collect();
        // Everything off the window, above it as well as below. Counting only
        // what follows would report "+1 more" for a plan with two finished
        // tasks scrolled off the top, which is a count of the wrong thing.
        let hidden = self.tasks.len().saturating_sub(rows.len());
        if hidden > 0 {
            rows.push(self.theme.dim.apply(&format!("  +{hidden} more")));
        }
        rows
    }

    /// What is waiting, drawn above the composer.
    ///
    /// Dimmed and caret-marked, so they read as "sent, not yet asked" beside
    /// the line being typed rather than as part of it.
    fn queued_rows(&self, width: usize) -> Vec<String> {
        if self.queued.is_empty() {
            return Vec::new();
        }
        let mut rows: Vec<String> = self
            .queued
            .iter()
            .take(QUEUED_ROWS)
            .map(|line| {
                let row = format!("  › {line}");
                self.theme.dim.apply(&truncate_to_width(&row, width, "…"))
            })
            .collect();
        let hidden = self.queued.len().saturating_sub(rows.len());
        if hidden > 0 {
            rows.push(self.theme.dim.apply(&format!("  +{hidden} more")));
        }
        rows
    }
}

/// One row of the plan: a mark for the status, then the text.
fn task_row(theme: &Theme, status: TaskStatus, text: &str) -> String {
    let (mark, styled) = match status {
        TaskStatus::Done => ("✓", theme.dim.apply(text)),
        TaskStatus::Doing => ("▸", theme.accent.apply(text)),
        TaskStatus::Todo => ("·", theme.dim.apply(text)),
    };
    format!("  {mark} {styled}")
}

/// The rows of a message, cut to the ones worth showing.
///
/// A message longer than the cap is shown from the caret backwards, because
/// the caret is where the typing is. Without this a long paste scrolls what is
/// being typed off the composer, and the cursor with it.
fn around_cursor(rows: Vec<String>, cap: Option<usize>) -> Vec<String> {
    let Some(cap) = cap else {
        return rows;
    };
    if rows.len() <= cap {
        return rows;
    }
    let caret = cursor_in(&rows).map_or(rows.len().saturating_sub(1), |(row, _)| row);
    let end = (caret + 1).max(cap).min(rows.len());
    rows[end - cap..end].to_vec()
}
