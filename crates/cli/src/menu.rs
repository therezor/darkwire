//! Opening a menu, and deciding whether this terminal can have one.
//!
//! A menu is rows in the same frame as the transcript and the editor, drawn by
//! the same renderer, so there is nothing to hand over and nothing to erase —
//! and, the reason it matters, no second set of row arithmetic to disagree with
//! the first when the window changes size.
//!
//! What is left here is the seam. The frame's owner supplies the opener,
//! because only that code knows where a menu goes among the rows and which
//! keystrokes should reach it; this module knows only that a menu can be shown
//! and eventually answers.
//!
//! [`NoMenu`] is the other half of the design. Every non-interactive path — a
//! pipe, `--json`, `TERM=dumb`, a one-shot, a test — gets it by construction
//! rather than by remembering an `if`, which is what makes "the scripted paths
//! are untouched" a property of the type rather than a convention.
//!
//! A menu answers with the *index* of the row that was chosen rather than with
//! a value of the caller's own type. That is what keeps [`Menu`] usable behind
//! a reference: a method generic over the row's type could not be, and every
//! caller already holds the list it offered.

use ghostai_tui::{SelectItem, SelectLabels, TerminalInput, TerminalOutput, columns_of};

use crate::i18n::Env;

/// A terminal narrower than this cannot hold a label and a cursor marker, so a
/// menu on it would be a column of ellipses.
pub const MIN_COLUMNS: usize = 12;

/// One row of a menu, already translated: the toolkit holds no keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MenuRow {
    /// The text drawn on the row.
    pub label: String,
    /// A dim right-hand column: a model id, a message count, `current`.
    pub hint: Option<String>,
    /// Matched by the filter but never drawn.
    pub keywords: Option<String>,
    /// Drawn dim and skipped by the cursor.
    pub disabled: bool,
}

impl MenuRow {
    /// A plain, enabled row with no hint.
    #[must_use]
    pub fn new(label: impl Into<String>) -> MenuRow {
        MenuRow {
            label: label.into(),
            ..MenuRow::default()
        }
    }

    /// The same row with a dim right-hand column.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> MenuRow {
        self.hint = Some(hint.into());
        self
    }

    /// The same row with extra text the filter matches but nothing draws.
    #[must_use]
    pub fn with_keywords(mut self, keywords: impl Into<String>) -> MenuRow {
        self.keywords = Some(keywords.into());
        self
    }

    /// The same row, drawn dim and skipped by the cursor.
    #[must_use]
    pub fn disabled(mut self) -> MenuRow {
        self.disabled = true;
        self
    }
}

/// One menu, as the frame's owner is asked to draw it.
#[derive(Debug, Clone)]
pub struct MenuRequest {
    /// The rows, in the order they are offered.
    pub rows: Vec<MenuRow>,
    /// The prose around them. Already translated.
    pub labels: SelectLabels,
    /// Where the cursor starts.
    pub index: Option<usize>,
}

impl MenuRequest {
    /// A menu over these rows, with the cursor on the first of them.
    #[must_use]
    pub fn new(rows: Vec<MenuRow>, labels: SelectLabels) -> MenuRequest {
        MenuRequest {
            rows,
            labels,
            index: None,
        }
    }

    /// The same menu, opened with the cursor on one row.
    #[must_use]
    pub fn at(mut self, index: usize) -> MenuRequest {
        self.index = Some(index);
        self
    }

    /// The rows as the toolkit's own item type, each carrying its own index.
    ///
    /// The conversion lives here rather than at the frame's owner so that "the
    /// value is the index" is stated once. An answer of `Some(3)` is the fourth
    /// row of [`MenuRequest::rows`], whatever the caller was offering.
    #[must_use]
    pub fn select_items(&self) -> Vec<SelectItem<usize>> {
        self.rows
            .iter()
            .enumerate()
            .map(|(index, row)| SelectItem {
                value: index,
                label: row.label.clone(),
                hint: row.hint.clone(),
                keywords: row.keywords.clone(),
                disabled: row.disabled,
            })
            .collect()
    }
}

/// Somewhere a menu can be shown.
pub trait Menu {
    /// `false` when there is no terminal to draw one on.
    fn available(&self) -> bool;

    /// The index of the chosen row, or `None` for a cancelled menu — and for
    /// every unavailable one.
    fn choose(&mut self, request: MenuRequest) -> Option<usize>;
}

/// Never draws, always answers nothing.
///
/// What every scripted path gets by construction, rather than by an `if`
/// somebody has to remember to write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoMenu;

impl Menu for NoMenu {
    fn available(&self) -> bool {
        false
    }

    fn choose(&mut self, request: MenuRequest) -> Option<usize> {
        let _ = request;
        None
    }
}

/// A menu drawn into the frame somebody else owns.
///
/// The opener puts the rows into the frame and answers when it closes. The
/// frame's owner supplies it, because only that code knows where a menu belongs
/// among the rows and which keystrokes should reach it.
pub struct FrameMenu<F> {
    open: F,
}

impl<F> FrameMenu<F>
where
    F: FnMut(MenuRequest) -> Option<usize>,
{
    /// A menu over one opener.
    pub fn new(open: F) -> FrameMenu<F> {
        FrameMenu { open }
    }
}

impl<F> std::fmt::Debug for FrameMenu<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FrameMenu")
    }
}

impl<F> Menu for FrameMenu<F>
where
    F: FnMut(MenuRequest) -> Option<usize>,
{
    fn available(&self) -> bool {
        true
    }

    fn choose(&mut self, request: MenuRequest) -> Option<usize> {
        (self.open)(request)
    }
}

/// What deciding whether a menu is possible needs to look at.
pub struct MenuAvailable<'a> {
    /// Where keystrokes would come from.
    pub input: &'a dyn TerminalInput,
    /// Where the rows would be drawn.
    pub output: &'a dyn TerminalOutput,
    /// `--json`: stdout carries one event per line and nothing else.
    pub json: bool,
    /// The environment, for `TERM`.
    pub env: &'a Env,
}

impl std::fmt::Debug for MenuAvailable<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuAvailable")
            .field("json", &self.json)
            .finish_non_exhaustive()
    }
}

/// Whether this invocation can draw a menu at all.
///
/// One predicate in one place, so the answer cannot differ between the picker
/// that asks and the code that decided to offer one. `TERM=dumb` is in here
/// because on a terminal that does not move a cursor, every escape sequence is
/// printed as literal text into the transcript. Emacs' `M-x shell` is the case
/// that actually happens.
#[must_use]
pub fn menu_available(options: &MenuAvailable<'_>) -> bool {
    if options.json {
        return false;
    }
    if !options.input.is_tty() || !options.output.is_tty() {
        return false;
    }
    if options.env.get("TERM") == Some("dumb") {
        return false;
    }
    // `columns_of` rather than a plain fallback on `None`, because a stream can
    // report zero and zero is not absent — a pty allocated by `script(1)` does
    // exactly that, and the naive spelling refuses to draw a menu on a terminal
    // that is fine.
    columns_of(options.output, None) >= MIN_COLUMNS
}
