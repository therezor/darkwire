//! A read-only overlay with tabs: keys in, rows out, nothing chosen.
//!
//! [`Select`] answers "which one?" and closes on a value. This answers
//! nothing. It is for the listings somebody opens to read, where the rows are
//! reference rather than a vocabulary, and where printing them into the
//! conversation would put a screenful of help between the question and the
//! answer to it, permanently, in the terminal's own history.
//!
//! It is a component and not a loop of its own, for the reason [`Select`] is:
//! rows in the same frame as everything else refold on a resize because the
//! renderer redraws the frame, and a region that painted itself would be a
//! second thing on screen for the renderer to disagree with.
//!
//! **Tabs rather than one long list.** The alternative is a page somebody
//! scrolls through looking for the section they wanted, which on a terminal is
//! worse than on paper: there is no scrollbar to say how much is left and no
//! way to jump. A tab is a named place.
//!
//! [`Select`]: crate::select::Select

use crate::component::Component;
use crate::keys::{Key, KeyName, is_ctrl};
use crate::text::{rule, truncate_to_width};
use crate::theme::{PLAIN_THEME, Theme};

/// What a cut row is marked with.
const ELLIPSIS: &str = "…";

/// The glyph the rule under the tabs is drawn with.
const RULE_GLYPH: &str = "─";

/// How many rows the chrome takes: the tab row, its rule, and the footer.
pub const CHROME_ROWS: usize = 3;

/// One tab and everything under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The tab's name. Already translated: this crate holds no keys.
    pub title: String,
    /// The rows, already laid out by whoever knows what they mean.
    pub rows: Vec<String>,
}

/// Everything the overlay says, already translated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagesLabels {
    /// The dim line under the rows, naming the keys.
    pub footer: String,
}

/// How the overlay is set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagesOptions {
    /// The tabs, in the order they are shown.
    pub pages: Vec<Page>,
    /// The prose around them.
    pub labels: PagesLabels,
    /// Defaults to [`PLAIN_THEME`], so a caller that forgets colour still
    /// reads.
    pub theme: Option<Theme>,
    /// Visible rows including the chrome, clamped by the caller to what the
    /// window can hold.
    pub max_rows: Option<usize>,
}

/// What a keystroke did to the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagesOutcome {
    /// Still open; render again.
    Open,
    /// The reader is done with it.
    Closed,
}

/// Tabs and the rows under the one that is showing.
#[derive(Debug)]
pub struct Pages {
    pages: Vec<Page>,
    labels: PagesLabels,
    theme: Theme,
    max_rows: usize,
    current: usize,
    /// The first visible row of the current page.
    top: usize,
}

impl Pages {
    /// An overlay over `options`.
    #[must_use]
    pub fn new(options: PagesOptions) -> Pages {
        Pages {
            pages: options.pages,
            labels: options.labels,
            theme: options.theme.unwrap_or(PLAIN_THEME),
            max_rows: options.max_rows.unwrap_or(20).max(CHROME_ROWS + 1),
            current: 0,
            top: 0,
        }
    }

    /// Which tab is showing.
    #[must_use]
    pub fn current(&self) -> usize {
        self.current
    }

    /// The first visible row of the tab that is showing.
    #[must_use]
    pub fn top(&self) -> usize {
        self.top
    }

    /// How many rows the body has room for.
    fn body_rows(&self) -> usize {
        self.max_rows.saturating_sub(CHROME_ROWS).max(1)
    }

    /// The rows of the tab that is showing.
    fn rows(&self) -> &[String] {
        self.pages
            .get(self.current)
            .map_or(&[][..], |page| &page.rows[..])
    }

    /// How far down this tab can be scrolled.
    fn last_top(&self) -> usize {
        self.rows().len().saturating_sub(self.body_rows())
    }

    /// Moves to another tab, from the top of it.
    ///
    /// The scroll is per overlay rather than per tab on purpose: a reader who
    /// comes back to a tab wants the start of it, not wherever they had got to
    /// before they went looking somewhere else.
    fn go_to(&mut self, at: usize) {
        if self.pages.is_empty() {
            return;
        }
        self.current = at % self.pages.len();
        self.top = 0;
    }

    /// One keystroke.
    ///
    /// Left and right move between tabs, and so do Tab and Shift-Tab: the
    /// arrows are what a reader reaches for and Tab is what the row of names
    /// looks like it wants. Up, down and the page keys scroll. Escape, `q` and
    /// Return all close, because there is nothing to choose and every one of
    /// them is somebody saying they are done.
    pub fn handle_key(&mut self, key: &Key) -> PagesOutcome {
        match key.name {
            KeyName::Escape | KeyName::Enter => return PagesOutcome::Closed,
            KeyName::Char if is_ctrl(key, 'c') || is_ctrl(key, 'd') => {
                return PagesOutcome::Closed;
            }
            KeyName::Char if !key.ctrl && key.character == "q" => return PagesOutcome::Closed,
            KeyName::Right => self.go_to(self.current + 1),
            KeyName::Left => self.go_to(self.current + self.pages.len().saturating_sub(1)),
            KeyName::Tab if key.shift => {
                self.go_to(self.current + self.pages.len().saturating_sub(1));
            }
            KeyName::Tab => self.go_to(self.current + 1),
            KeyName::Down => self.top = (self.top + 1).min(self.last_top()),
            KeyName::Up => self.top = self.top.saturating_sub(1),
            KeyName::PageDown => self.top = (self.top + self.body_rows()).min(self.last_top()),
            KeyName::PageUp => self.top = self.top.saturating_sub(self.body_rows()),
            KeyName::Home => self.top = 0,
            KeyName::End => self.top = self.last_top(),
            _ => {}
        }
        PagesOutcome::Open
    }

    /// The row of tab names, with the one showing marked.
    fn tab_row(&self, width: usize) -> String {
        let names: Vec<String> = self
            .pages
            .iter()
            .enumerate()
            .map(|(at, page)| {
                if at == self.current {
                    self.theme.accent.apply(&format!(" {} ", page.title))
                } else {
                    self.theme.dim.apply(&format!(" {} ", page.title))
                }
            })
            .collect();
        truncate_to_width(&format!("  {}", names.join("")), width, ELLIPSIS)
    }

    /// The dim line under the rows, with where the reader is in this tab.
    fn footer_row(&self, width: usize) -> String {
        let rows = self.rows().len();
        let body = self.body_rows();
        // Only when there is something off screen. A position counter over a
        // page that fits is a number that never changes.
        let position = if rows > body {
            format!(
                "  {}–{} of {rows}",
                self.top + 1,
                (self.top + body).min(rows)
            )
        } else {
            String::new()
        };
        let text = format!("  {}{position}", self.labels.footer);
        truncate_to_width(&self.theme.dim.apply(&text), width, ELLIPSIS)
    }
}

impl Component for Pages {
    fn render(&mut self, width: usize) -> Vec<String> {
        // Clamped here as well as at the key map, because the window can shrink
        // under an overlay that is already open and a `top` past the end draws
        // nothing at all.
        self.top = self.top.min(self.last_top());

        let body = self.body_rows();
        let mut out = Vec::with_capacity(body + CHROME_ROWS);
        out.push(self.tab_row(width));
        out.push(self.theme.dim.apply(&rule(width, RULE_GLYPH)));
        for row in self.rows().iter().skip(self.top).take(body) {
            out.push(truncate_to_width(row, width, ELLIPSIS));
        }
        // Padded to the full height so the rows below the overlay do not move
        // when the reader changes to a shorter tab.
        let drawn = out.len().saturating_sub(2);
        for _ in drawn..body {
            out.push(String::new());
        }
        out.push(self.footer_row(width));
        out
    }
}
