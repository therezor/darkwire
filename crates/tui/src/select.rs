//! A menu: keys in, one value or nothing out.
//!
//! Short, because [`SelectList`] owns every decision about what is selected
//! and the renderer owns every byte. What is left is a key map and a frame.
//!
//! It is a component and not a loop of its own. A menu that opened its own
//! region, took the keyboard and painted itself would be a second thing on
//! screen for the renderer to disagree with; as rows in the same frame as
//! everything else it refolds on a resize for the same reason the transcript
//! does.
//!
//! **Cancelling answers [`SelectOutcome::Cancelled`] rather than an error.**
//! The operator pressed Escape, which is a perfectly ordinary answer to "which
//! agent?"; an error would make every call site handle it as a failure.
//!
//! **Ctrl-P/Ctrl-N move as well as the arrow keys.** They are plain control
//! bytes, which makes them the only movement keys that survive a terminal whose
//! cursor sequences arrive in a form nothing recognises — legacy Windows
//! conhost being the case that actually happens.

use crate::component::Component;
use crate::keys::{Key, KeyName, is_ctrl};
use crate::select_list::{SelectItem, SelectList};
use crate::text::truncate_to_width;
use crate::theme::{PLAIN_THEME, Theme};

/// Everything a menu says. Already translated: this crate holds no keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectLabels {
    /// The heading.
    pub title: String,
    /// Shown in place of the rows when the filter matches nothing.
    pub empty: String,
    /// The dim line under the rows, naming the keys.
    pub footer: String,
    /// Drawn before the filter text. Default `/`.
    pub filter_prefix: Option<String>,
}

/// How a menu is set up.
pub struct SelectOptions<T> {
    /// The rows.
    pub items: Vec<SelectItem<T>>,
    /// The prose around them.
    pub labels: SelectLabels,
    /// Defaults to [`PLAIN_THEME`], so a caller that forgets colour still
    /// reads.
    pub theme: Option<Theme>,
    /// Where the cursor starts.
    pub index: Option<usize>,
    /// Visible rows, clamped by the caller to what the window can hold.
    pub max_rows: Option<usize>,
}

/// What a keystroke did to the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectOutcome<T> {
    /// Still open; render again.
    Open,
    /// A row was chosen.
    Chosen(T),
    /// The operator left without choosing.
    Cancelled,
}

/// How many rows a menu shows when the caller does not say.
pub const DEFAULT_MAX_ROWS: usize = 10;
/// Title, filter, footer, the scroll counter, and a row of breathing space.
pub const CHROME_ROWS: usize = 5;
const DEFAULT_FILTER_PREFIX: &str = "/";

/// A menu over a [`SelectList`].
pub struct Select<T> {
    labels: SelectLabels,
    theme: Theme,
    list: SelectList<T>,
}

impl<T: Clone> Select<T> {
    /// A menu with the cursor on `options.index`, or the first row.
    pub fn new(options: SelectOptions<T>) -> Self {
        let list = SelectList::new(
            options.items,
            Some(options.max_rows.unwrap_or(DEFAULT_MAX_ROWS)),
            options.index,
        );
        Self {
            labels: options.labels,
            theme: options.theme.unwrap_or(PLAIN_THEME),
            list,
        }
    }

    /// Clamps the list to what the window can spare.
    pub fn set_rows(&mut self, rows: usize) {
        self.list.set_rows(rows);
    }

    /// The list underneath, for a caller that wants to look at it.
    pub fn list(&self) -> &SelectList<T> {
        &self.list
    }

    /// Applies a keystroke.
    pub fn handle_key(&mut self, key: &Key) -> SelectOutcome<T> {
        if key.name == KeyName::Escape || is_ctrl(key, 'c') || is_ctrl(key, 'd') {
            return SelectOutcome::Cancelled;
        }

        if key.name == KeyName::Enter {
            // Nothing matched: Enter means "give up" rather than "choose the
            // row that is not there". A disabled row is different — it is on
            // screen, so the key doing nothing is the honest answer.
            return match self.list.selected() {
                None => SelectOutcome::Cancelled,
                Some(item) if item.disabled => SelectOutcome::Open,
                Some(item) => SelectOutcome::Chosen(item.value.clone()),
            };
        }

        let rows = i64::try_from(self.list.rows()).unwrap_or(i64::MAX);
        match key.name {
            KeyName::Up => self.list.move_up(),
            KeyName::Tab if key.shift => self.list.move_up(),
            KeyName::Down | KeyName::Tab => self.list.move_down(),
            KeyName::PageUp => self.list.move_by(-rows),
            KeyName::PageDown => self.list.move_by(rows),
            KeyName::Home => self.list.first(),
            KeyName::End => self.list.last(),
            KeyName::Backspace => {
                let shortened = crate::text::drop_last_grapheme(self.list.filter());
                self.list.set_filter(&shortened);
            }
            KeyName::Char if is_ctrl(key, 'p') => self.list.move_up(),
            KeyName::Char if is_ctrl(key, 'n') => self.list.move_down(),
            KeyName::Char if is_ctrl(key, 'u') => self.list.set_filter(""),
            KeyName::Char if !key.ctrl && !key.meta => {
                let extended = format!("{}{}", self.list.filter(), key.character);
                self.list.set_filter(&extended);
            }
            _ => {}
        }

        SelectOutcome::Open
    }
}

impl<T: Clone> Component for Select<T> {
    fn render(&mut self, width: usize) -> Vec<String> {
        let fit = |text: &str| truncate_to_width(text, width, "…");
        let prefix = self
            .labels
            .filter_prefix
            .as_deref()
            .unwrap_or(DEFAULT_FILTER_PREFIX);
        let theme = &self.theme;
        let rows = self.list.render(width, theme);

        let mut lines = vec![
            theme.title.apply(&fit(&self.labels.title)),
            theme
                .dim
                .apply(&fit(&format!("{prefix}{}", self.list.filter()))),
        ];
        if rows.is_empty() {
            lines.push(theme.dim.apply(&fit(&format!("  {}", self.labels.empty))));
        } else {
            lines.extend(rows);
        }
        lines.push(theme.dim.apply(&fit(&format!("  {}", self.labels.footer))));
        lines
    }
}
