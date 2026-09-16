//! Sessions, as rows.
//!
//! `/sessions` opens this on a terminal and prints its listing on a pipe — the
//! shape `/agent` and `/workspace` already take, so there is one rule for all
//! three rather than a picker verb beside a listing verb for each of them. The
//! numeric argument is the page size in both cases, which is what keeps a script
//! and a person asking the same question of the same rows.

use darkwire_core::session_store::SessionSummaryRecord;
use darkwire_i18n::{args, keys};
use darkwire_tui::SelectItem;

use crate::i18n::Translations;
use crate::pickers::{PickerMenu, choose_from, position_of, with_current};

/// One row per session, newest first — the order the store already returns.
///
/// The key goes in the hint rather than the label: a title is what a person
/// recognises, and `cli-9f2ab1` beside it is what they need only when two
/// conversations share a name.
pub fn session_items(
    sessions: &[SessionSummaryRecord],
    current: &str,
    t: &Translations,
) -> Vec<SelectItem<String>> {
    sessions
        .iter()
        .map(|record| {
            // A `{{count}}` and a one/other pair: a hint is two words, and a
            // locale-aware separator on a number that is almost always single
            // digits buys nothing.
            let count = t.tr(keys::menu::MESSAGES, args!["count" => record.message_count]);
            let hint = if record.session.key == current {
                with_current(&count, t)
            } else {
                count
            };
            SelectItem {
                value: record.session.key.clone(),
                label: if record.session.title.is_empty() {
                    record.session.key.clone()
                } else {
                    record.session.title.clone()
                },
                hint: Some(hint),
                keywords: Some(record.session.key.clone()),
                disabled: false,
            }
        })
        .collect()
}

/// Opens the menu on the current conversation. `None` if it was cancelled.
pub async fn pick_session(
    menu: &dyn PickerMenu,
    sessions: &[SessionSummaryRecord],
    current: &str,
    t: &Translations,
) -> Option<String> {
    let items = session_items(sessions, current, t);
    let at = position_of(&items, &current.to_owned());
    choose_from(menu, items, &t.t(keys::menu::titles::SESSION), at, t).await
}
