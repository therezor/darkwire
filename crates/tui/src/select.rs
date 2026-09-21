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
//!
//! **A row can offer verbs besides being chosen**, through [`SelectAction`].
//! They are control chords and not plain letters, because every plain letter is
//! already spoken for: it goes into the filter. A menu of tasks with a `d` for
//! delete is a menu you cannot type `d` into.

use crate::component::{CURSOR_MARKER, Component};
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
    /// Drawn between the title and the filter text. Default a single space.
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
    /// The verbs a row offers. Empty for a menu that only chooses.
    pub actions: Vec<SelectAction>,
}

/// A verb a row offers, beside being chosen.
///
/// The chord is a letter that is fired with Control held. `p`, `n` and `u` are
/// already bound here and a menu that reuses one loses the movement key rather
/// than gaining a verb, so [`Select`] ignores an action that names one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectAction {
    /// The letter, held with Control.
    pub chord: char,
    /// Already translated, for the footer.
    pub label: String,
}

/// The chords the menu keeps for itself.
const BOUND_CHORDS: &[char] = &['c', 'd', 'n', 'p', 'u'];

/// What a keystroke did to the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectOutcome<T> {
    /// Still open; render again.
    Open,
    /// A row was chosen.
    Chosen(T),
    /// A verb was fired on a row.
    Acted {
        /// Which of the menu's actions, by position.
        action: usize,
        /// The row it was fired on.
        value: T,
    },
    /// The operator left without choosing.
    Cancelled,
}

/// How many rows a menu shows when the caller does not say.
pub const DEFAULT_MAX_ROWS: usize = 10;
/// The footer, and nothing else.
///
/// The title and the filter are one row that the host draws where it draws its
/// own prompt, because a menu opened from a prompt has a prompt already and a
/// second empty one under it is a row spent saying nothing. The scroll counter
/// shares the footer with the keys for the same reason.
pub const CHROME_ROWS: usize = 1;
const DEFAULT_FILTER_PREFIX: &str = " ";

/// A menu over a [`SelectList`].
pub struct Select<T> {
    labels: SelectLabels,
    theme: Theme,
    list: SelectList<T>,
    actions: Vec<SelectAction>,
}

impl<T: Clone> Select<T> {
    /// A menu with the cursor on `options.index`, or the first row.
    pub fn new(options: SelectOptions<T>) -> Self {
        let list = SelectList::new(
            options.items,
            Some(options.max_rows.unwrap_or(DEFAULT_MAX_ROWS)),
            options.index,
        );
        let actions = options
            .actions
            .into_iter()
            .filter(|action| !BOUND_CHORDS.contains(&action.chord))
            .collect();
        Self {
            labels: options.labels,
            theme: options.theme.unwrap_or(PLAIN_THEME),
            list,
            actions,
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

    /// The row the question and the filter share, with the caret in it.
    ///
    /// Handed out rather than rendered, because where it goes is the host's to
    /// decide: a menu in a prompt puts it on the prompt's own line, and a menu
    /// that took the window puts it at the top of one.
    #[must_use]
    pub fn prompt(&self) -> String {
        let separator = self
            .labels
            .filter_prefix
            .as_deref()
            .unwrap_or(DEFAULT_FILTER_PREFIX);
        format!(
            "{}{separator}{}{CURSOR_MARKER}",
            self.theme.title.apply(&self.labels.title),
            self.list.filter()
        )
    }

    /// The keys, with the scroll counter in front of them when there is one.
    fn footer(&self) -> String {
        let keys = self.labels.footer.clone();
        let keys = self.actions.iter().fold(keys, |so_far, action| {
            format!("{so_far} · ^{} {}", action.chord, action.label)
        });
        match self.list.counter() {
            None => format!("  {keys}"),
            Some((at, total)) => format!("  ({at}/{total}) · {keys}"),
        }
    }

    /// Which action a chord fires, if any.
    fn action_for(&self, key: &Key) -> Option<usize> {
        self.actions
            .iter()
            .position(|action| is_ctrl(key, action.chord))
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

        // Ahead of everything but leaving, so a menu cannot bind a chord that
        // quietly stops working when this file grows a key. `new` has already
        // dropped any action naming a chord below.
        if let Some(action) = self.action_for(key) {
            return match self.list.selected() {
                Some(item) if !item.disabled => SelectOutcome::Acted {
                    action,
                    value: item.value.clone(),
                },
                // No row, or a row that is on screen and refusing: the key
                // doing nothing is the honest answer, as it is for Enter.
                _ => SelectOutcome::Open,
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
        let theme = &self.theme;
        let rows = self.list.render(width, theme);

        let mut lines = Vec::new();
        if rows.is_empty() {
            lines.push(theme.dim.apply(&fit(&format!("  {}", self.labels.empty))));
        } else {
            lines.extend(rows);
        }
        lines.push(theme.dim.apply(&fit(&self.footer())));
        lines
    }
}
