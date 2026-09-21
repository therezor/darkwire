//! Sessions, as rows.
//!
//! `/session` opens this on a terminal and prints its listing on a pipe — the
//! shape `/agent` and `/workspace` already take, so there is one rule for all
//! three rather than a picker verb beside a listing verb for each of them.
//!
//! **Deleting one is a verb here.** `/delete <key>` used to do it, which put
//! the most destructive thing the prompt can do behind the shortest word and a
//! key nobody has memorised. On the row, the conversation being deleted is
//! named, dated and counted in front of you.

use darkwire_core::session_store::SessionSummaryRecord;
use darkwire_i18n::{args, keys};
use darkwire_tui::{SelectAction, SelectItem};

use crate::i18n::Translations;
use crate::pickers::{PickerMenu, Placement, act_on, position_of, with_current};

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

/// What was asked of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionVerb {
    /// Delete it, and everything said in it.
    Delete,
}

impl SessionVerb {
    /// The verbs the menu offers, in the order their keys are listed.
    const ALL: [SessionVerb; 1] = [SessionVerb::Delete];

    /// The letter it is fired with, held under Control.
    fn chord(self) -> char {
        match self {
            SessionVerb::Delete => 'x',
        }
    }

    fn label(self, t: &Translations) -> String {
        match self {
            SessionVerb::Delete => t.t(keys::menu::actions::DELETE_SESSION),
        }
    }
}

/// The verbs the menu offers, already translated.
pub fn session_actions(t: &Translations) -> Vec<SelectAction> {
    SessionVerb::ALL
        .iter()
        .map(|verb| SelectAction {
            chord: verb.chord(),
            label: verb.label(t),
        })
        .collect()
}

/// Opens the menu on the current conversation. `None` if it was cancelled.
///
/// Answers the key, and the verb fired on it if there was one.
pub async fn pick_session(
    menu: &dyn PickerMenu,
    sessions: &[SessionSummaryRecord],
    current: &str,
    t: &Translations,
) -> Option<(String, Option<SessionVerb>)> {
    let items = session_items(sessions, current, t);
    let at = position_of(&items, &current.to_owned());
    let (key, action) = act_on(
        menu,
        items,
        &t.t(keys::menu::titles::SESSION),
        at,
        t,
        Placement::Window,
        session_actions(t),
    )
    .await?;
    Some((key, action.and_then(|at| SessionVerb::ALL.get(at).copied())))
}
