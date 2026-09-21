//! The menus the REPL opens, and the rows behind them.
//!
//! Every picker here is split in two on purpose. A `*_items` function is a pure
//! function from domain records to rows — all the labelling, the marking of the
//! current one and every translated string live in it, and it tests with three
//! structs and no terminal. A `pick_*` function is the handful of lines that
//! hand those rows to a menu. Most of what could be wrong with a picker is in
//! the first half, and the first half needs nothing to run.
//!
//! None of them takes a runtime. A picker holding one would be a picker that
//! could run a turn, and the point of this layer is that it cannot do anything
//! except turn data into a choice.
//!
//! A terminal that cannot draw a menu — a pipe, `--json`, `TERM=dumb` — still
//! deserves an answer, so the pickers that have a list to show also have a
//! `*_listing`, which is the same rows as text with the current one marked.

pub mod agents;
pub mod effort;
pub mod memories;
pub mod models;
pub mod palette;
pub mod sessions;
pub mod skills;
pub mod tasks;
pub mod workspaces;

use std::future::Future;
use std::pin::Pin;

use darkwire_tui::{Page, PagesLabels, SelectAction, SelectItem, SelectLabels};

use crate::i18n::Translations;

/// What a picker asks a menu to show.
///
/// The rows are addressed by position rather than by value, which is what makes
/// the trait below usable as `dyn`: a method generic over the row's type cannot
/// be. Each picker builds rows of its own value type for the pure half, and
/// converts to positions at the one call that opens a menu.
#[derive(Debug)]
pub struct MenuRequest {
    /// The rows, each valued with its own index.
    pub items: Vec<SelectItem<usize>>,
    /// Already translated: the toolkit holds no keys.
    pub labels: SelectLabels,
    /// Where the cursor starts.
    pub index: Option<usize>,
    /// How much of the screen it is worth.
    pub placement: Placement,
    /// The verbs a row offers. Empty for a menu that only chooses.
    pub actions: Vec<SelectAction>,
}

/// What came back from a menu.
///
/// A row, and which of the menu's verbs was fired on it. `None` for the verb
/// means the row was simply chosen, which is what every menu without actions
/// ever answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuAnswer {
    /// Which row, by position.
    pub row: usize,
    /// Which action, by position among the request's.
    pub action: Option<usize>,
}

/// Where a menu is drawn.
///
/// A short vocabulary is read at a glance and belongs beside the prompt that
/// asked for it, where the conversation above stays visible. A long list is
/// searched instead of read, and five rows of it is a list you filter blind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// In the rows under the composer.
    #[default]
    Prompt,
    /// Over the whole window, the way a listing goes.
    Window,
}

/// What a question asks a frame to put on the window.
///
/// The answer is a line of text, or nothing for a question somebody left. An
/// empty line is an answer, and refusing it is the caller's business: only the
/// caller knows whether a blank name is a mistake or a way of saying "no
/// change".
#[derive(Debug)]
pub struct AskRequest {
    /// The question, already translated.
    pub title: String,
    /// Already on the line when it opens, for a rename.
    pub initial: String,
}

/// What a listing asks a frame to show.
///
/// Nothing comes back. A listing is opened to be read, so the only thing the
/// caller learns is that the reader closed it.
#[derive(Debug)]
pub struct ListingRequest {
    /// The tabs, in the order they are shown.
    pub pages: Vec<Page>,
    /// Already translated: the toolkit holds no keys.
    pub labels: PagesLabels,
}

/// What a picker needs from whoever owns the frame.
///
/// The concrete menu lives in `menu.rs`, which knows where a menu goes among
/// the rows of a frame and which keystrokes reach it. This is the narrow half
/// of it a picker uses, stated here so nothing in this module has to know that
/// a frame exists.
pub trait PickerMenu: Send + Sync {
    /// `false` when there is no terminal to draw one on.
    fn available(&self) -> bool;

    /// Puts the menu into the frame and answers when it closes.
    ///
    /// `None` for a cancelled menu, and for every unavailable one.
    fn choose<'a>(
        &'a self,
        request: MenuRequest,
    ) -> Pin<Box<dyn Future<Output = Option<MenuAnswer>> + Send + 'a>>;

    /// Puts a question on the window and answers with the line typed into it.
    ///
    /// `None` for a question that was left, and for every unavailable one —
    /// which is why the commands that ask one never open where there is no
    /// screen to draw it on.
    fn ask<'a>(
        &'a self,
        request: AskRequest,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>>;

    /// Puts a listing into the frame and answers when it closes.
    ///
    /// `false` when there was nowhere to draw one, which is what tells a caller
    /// to write the same rows to the stream instead. A pipe has no overlay and
    /// still deserves the answer.
    fn show<'a>(
        &'a self,
        request: ListingRequest,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
}

/// A menu that never draws and always answers nothing.
///
/// Every non-interactive path gets it by construction rather than by
/// remembering an `if`, which is what makes "the scripted paths are untouched"
/// a property of the type rather than a convention.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoMenu;

impl PickerMenu for NoMenu {
    fn available(&self) -> bool {
        false
    }

    fn choose<'a>(
        &'a self,
        request: MenuRequest,
    ) -> Pin<Box<dyn Future<Output = Option<MenuAnswer>> + Send + 'a>> {
        drop(request);
        Box::pin(std::future::ready(None))
    }

    fn ask<'a>(
        &'a self,
        request: AskRequest,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> {
        drop(request);
        Box::pin(std::future::ready(None))
    }

    fn show<'a>(
        &'a self,
        request: ListingRequest,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        drop(request);
        Box::pin(std::future::ready(false))
    }
}

/// The same rows, valued with their own positions.
fn indexed<T>(items: &[SelectItem<T>]) -> Vec<SelectItem<usize>> {
    items
        .iter()
        .enumerate()
        .map(|(at, item)| SelectItem {
            value: at,
            label: item.label.clone(),
            hint: item.hint.clone(),
            keywords: item.keywords.clone(),
            disabled: item.disabled,
        })
        .collect()
}

/// The prose every menu in this module carries, differing only in its title.
pub fn labels(title: &str, t: &Translations) -> SelectLabels {
    SelectLabels {
        title: title.to_owned(),
        empty: t.t(darkwire_i18n::keys::menu::EMPTY),
        footer: t.t(darkwire_i18n::keys::menu::FOOTER),
        filter_prefix: None,
    }
}

/// Opens `menu` on `items` and answers with the chosen row's value.
///
/// The one place positions become values again, so no picker has to write the
/// conversion and none of them can get it wrong in a different way.
pub(crate) async fn choose_from<T: Clone>(
    menu: &dyn PickerMenu,
    items: Vec<SelectItem<T>>,
    title: &str,
    index: Option<usize>,
    t: &Translations,
    placement: Placement,
) -> Option<T> {
    let (value, _) = act_on(menu, items, title, index, t, placement, Vec::new()).await?;
    Some(value)
}

/// The same, for a menu whose rows offer verbs.
///
/// Answers the row's value and which verb was fired on it, `None` for a row
/// that was simply chosen. The caller applies the verb and decides whether to
/// open the menu again: this layer holds no store, and a picker that could
/// write to one would be a picker that could run a turn.
pub(crate) async fn act_on<T: Clone>(
    menu: &dyn PickerMenu,
    items: Vec<SelectItem<T>>,
    title: &str,
    index: Option<usize>,
    t: &Translations,
    placement: Placement,
    actions: Vec<SelectAction>,
) -> Option<(T, Option<usize>)> {
    let request = MenuRequest {
        items: indexed(&items),
        labels: labels(title, t),
        index,
        placement,
        actions,
    };
    let answer = menu.choose(request).await?;
    let value = items.get(answer.row).map(|item| item.value.clone())?;
    Some((value, answer.action))
}

/// Where a value sits among the rows, for opening the menu on it.
fn position_of<T: PartialEq>(items: &[SelectItem<T>], value: &T) -> Option<usize> {
    items.iter().position(|item| &item.value == value)
}

/// The hint a row carries when it is the one in force, appended to whatever it
/// already said.
fn with_current(hint: &str, t: &Translations) -> String {
    let current = t.t(darkwire_i18n::keys::menu::CURRENT);
    if hint.is_empty() {
        current
    } else {
        format!("{hint} · {current}")
    }
}
