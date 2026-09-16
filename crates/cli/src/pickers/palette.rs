//! Every slash command, as one searchable list.
//!
//! The rows come from the slash commands' own help table rather than a second
//! list beside it — one table, so a command cannot exist in the palette and not
//! in `/help`, or the other way round. The arrow points one way: this module is
//! handed the rows and never reaches back for them.
//!
//! [`PaletteRow`] is the narrow view of a command this module needs: the syntax
//! line and the key of its description. The full row lives with the commands,
//! which also carry a handler and a section; converting at the boundary is what
//! keeps this file free of everything a command can *do*.
//!
//! **A row that needs an argument is typed, not run.** `/rename <title>`
//! submitted on its own is a usage error the operator has to read and then
//! retype around, so the palette puts `/rename ` in the editor and leaves the
//! cursor after it. A row that needs nothing — `/help`, `/agent`, `/context` —
//! is submitted outright, because there is nothing left to say.

use darkwire_i18n::keys;
use darkwire_tui::SelectItem;

use crate::i18n::Translations;
use crate::pickers::{PickerMenu, choose_from};

/// One command, as the palette needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteRow {
    /// The syntax line: `/workspace move <from> <to>`, `/exit, /quit`.
    pub syntax: String,
    /// The bundle key of its one-line description, when it has one.
    ///
    /// A variant row — `/workspace new <name>` beside `/workspace` — carries
    /// none rather than an invented sentence.
    pub key: Option<&'static str>,
}

impl PaletteRow {
    /// A row with a description.
    pub fn new(syntax: &str, key: &'static str) -> PaletteRow {
        PaletteRow {
            syntax: syntax.to_owned(),
            key: Some(key),
        }
    }

    /// A variant row, which has no description of its own.
    pub fn variant(syntax: &str) -> PaletteRow {
        PaletteRow {
            syntax: syntax.to_owned(),
            key: None,
        }
    }
}

/// What choosing a palette row does to the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandChoice {
    /// What to put on the line: `/workspace move`, not the placeholders.
    pub command: String,
    /// Whether to press Return, or leave the operator typing the arguments.
    pub submit: bool,
}

/// The typeable part of a row's syntax.
///
/// `/workspace move <from> <to>` becomes `/workspace move`, `/agent [id]`
/// becomes `/agent`, `/exit, /quit` becomes `/exit`. Placeholders end it, and so
/// does the comma that separates a command from its alias — an alias is the same
/// command, and offering both would double a list whose whole value is being
/// short.
pub fn command_value(syntax: &str) -> String {
    let mut words: Vec<&str> = Vec::new();
    for word in syntax.split_whitespace() {
        if word.starts_with('<') || word.starts_with('[') {
            break;
        }
        if let Some(trimmed) = word.strip_suffix(',') {
            words.push(trimmed);
            break;
        }
        words.push(word);
    }
    words.join(" ")
}

/// Whether a row cannot run without something the operator has yet to type.
fn needs_argument(syntax: &str) -> bool {
    syntax.contains('<')
}

/// One row per command, labelled with its syntax.
pub fn command_items(rows: &[PaletteRow], t: &Translations) -> Vec<SelectItem<CommandChoice>> {
    rows.iter()
        .map(|row| SelectItem {
            value: CommandChoice {
                command: command_value(&row.syntax),
                submit: !needs_argument(&row.syntax),
            },
            label: row.syntax.clone(),
            hint: row.key.map(|key| t.t(key)),
            keywords: None,
            disabled: false,
        })
        .collect()
}

/// Opens the palette. `None` if it was cancelled.
pub async fn pick_command(
    menu: &dyn PickerMenu,
    rows: &[PaletteRow],
    t: &Translations,
) -> Option<CommandChoice> {
    let items = command_items(rows, t);
    choose_from(menu, items, &t.t(keys::menu::titles::COMMAND), None, t).await
}

/// Tab completion over the same table the help page uses.
///
/// Only ever completes a slash command: a prompt is mostly prose, and a
/// completer that guessed at the middle of a sentence would be a Tab key that
/// inserted something surprising far more often than it helped.
///
/// Answers with the candidates and the text they complete, which is the pair an
/// editor needs to replace what was typed.
pub fn complete_command(line: &str, rows: &[PaletteRow]) -> (Vec<String>, String) {
    if !line.starts_with('/') {
        return (Vec::new(), line.to_owned());
    }
    let mut seen: Vec<String> = Vec::new();
    for row in rows {
        let value = command_value(&row.syntax);
        // Each command once, however many rows describe it: `/workspace` has
        // five syntax lines and is one thing to complete to.
        if value.starts_with(line) && !seen.contains(&value) {
            seen.push(value);
        }
    }
    (seen, line.to_owned())
}
