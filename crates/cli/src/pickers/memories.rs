//! What a workspace remembers, as rows with the switch under them.
//!
//! The rows are the index exactly as every prompt on this folder carries it,
//! line for line — which is the point: this answers "what does the model know
//! about this place", and the honest answer is the text it is actually sent.
//!
//! **The switch is here rather than a command.** `/memory on` and `/memory off`
//! said what a key beside the list says, and the list is where somebody
//! deciding whether to keep remembering is already looking.

use darkwire_core::memory::{Memory, index_line};
use darkwire_i18n::keys;
use darkwire_tui::{SelectAction, SelectItem};

use crate::i18n::Translations;
use crate::pickers::{PickerMenu, Placement, act_on};

/// The verb the list offers, as the only one.
const TOGGLE: usize = 0;

/// One row per memory, valued with its position, and the summary above them.
///
/// The summary is a disabled first row rather than a tab of its own: it is one
/// line, and a tab holding one line is a tab somebody has to go and find.
pub fn memory_items(memories: &[Memory], summary: &str) -> Vec<SelectItem<usize>> {
    let mut items = vec![SelectItem {
        value: usize::MAX,
        label: summary.to_owned(),
        hint: None,
        keywords: None,
        disabled: true,
    }];
    items.extend(memories.iter().enumerate().map(|(at, memory)| SelectItem {
        value: at,
        label: index_line(memory),
        hint: None,
        keywords: None,
        disabled: false,
    }));
    items
}

/// The verb the menu offers, worded for the state it would leave things in.
pub fn memory_actions(granted: bool, t: &Translations) -> Vec<SelectAction> {
    vec![SelectAction {
        chord: 'g',
        label: t.t(if granted {
            keys::menu::actions::FORGET
        } else {
            keys::menu::actions::REMEMBER
        }),
    }]
}

/// What the index was closed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryChoice {
    /// Read this one, by position in the list.
    ///
    /// The index line is a key and a title, which is a reminder of a memory
    /// rather than the memory. What the model is actually sent is the file, so
    /// choosing a row opens it.
    Read(usize),
    /// Flip whether the agent may remember at all.
    Toggle,
}

/// Opens the index over the window, at the row `at`.
///
/// `at` is where the cursor starts, so coming back from reading one lands on
/// the row it was read from rather than at the top.
pub async fn show_memories(
    menu: &dyn PickerMenu,
    memories: &[Memory],
    summary: &str,
    granted: bool,
    at: Option<usize>,
    t: &Translations,
) -> Option<MemoryChoice> {
    let (row, action) = act_on(
        menu,
        memory_items(memories, summary),
        &t.t(keys::menu::titles::MEMORY),
        at.map(|at| at.saturating_add(1)),
        t,
        Placement::Window,
        memory_actions(granted, t),
    )
    .await?;
    if action == Some(TOGGLE) {
        return Some(MemoryChoice::Toggle);
    }
    // The summary sits on a disabled first row, so it can never be chosen and
    // `usize::MAX` never comes back here. Guarded anyway: a value that cannot
    // happen is one refactor away from happening.
    (row != usize::MAX).then_some(MemoryChoice::Read(row))
}
