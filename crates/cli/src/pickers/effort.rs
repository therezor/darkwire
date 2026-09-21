//! Reasoning effort, as rows.
//!
//! The one picker whose list is a *type* rather than a catalogue. Everything
//! else in this folder turns records the runtime holds into rows; this turns an
//! enum into them, and the split-in-two shape is the same.
//!
//! [`LEVELS`] repeats the enum's own order because the protocol crate publishes
//! no list of its variants. What stops that repetition going stale is
//! [`effort_value`], whose `match` is exhaustive: a level added to the enum is a
//! compile error here before it is a row missing from the menu. That is the
//! Rust shape of reading the list off the schema.
//!
//! **`default` is a row, not a level.** It is what "send no reasoning parameter
//! at all" is called at the prompt, and it has to be pickable for the same
//! reason it has to be typeable: unset and `off` are different requests, and a
//! menu that only offered the six levels would make one of them unreachable
//! without going to the settings panel. Its value is the literal word, so a
//! level chosen from the menu and one typed after the command take the same
//! path.
//!
//! The hints are deliberately thin. Which levels mean anything is the model's
//! business rather than this project's — `minimal` is OpenAI's and `xhigh` is
//! Qwen3.8's top rung — so the two rows that carry a hint are the two whose
//! *mechanism* differs, and the rest say only whether they are the one in
//! force.

use darkwire_i18n::keys;
use darkwire_protocol::ReasoningEffort;
use darkwire_tui::SelectItem;

use crate::i18n::Translations;
use crate::pickers::{PickerMenu, Placement, choose_from, position_of, with_current};

/// The word for "send nothing and let the provider decide".
///
/// `default` names the outcome, and the outcome is the same on every agent: an
/// entry that states no effort sends no reasoning parameter, so what arrives is
/// the provider's own default. There is nothing above an agent for the word to
/// be ambiguous about — which is what makes it available at all.
///
/// Defined here because this is where it has to exist as *data*, a menu row
/// needs a value, and read by the slash commands as a word. `/temperature
/// default` reads it too: one spelling, one definition, so the menu and the
/// parser cannot drift apart on it.
///
/// Not translated, for the reason a command name is not: it is what an operator
/// types, so it is syntax.
pub const DEFAULT_LEVEL: &str = "default";

/// Every level, in the order the enum declares them.
///
/// Kept honest by [`effort_value`] rather than by a comment: adding a variant
/// without adding it here fails to compile there.
pub const LEVELS: [ReasoningEffort; 6] = [
    ReasoningEffort::Off,
    ReasoningEffort::Minimal,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::Xhigh,
];

/// The word one level goes to the wire as, and the word an operator types.
///
/// Exhaustive on purpose. This is what makes [`LEVELS`] impossible to leave
/// incomplete: a new variant has no arm here, and the crate does not build.
pub fn effort_value(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Off => "off",
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
    }
}

/// One level, read back from what an operator typed or a menu answered.
///
/// `default` is deliberately not a level: it is the absence of one, and the
/// caller distinguishes the two.
pub fn parse_effort(value: &str) -> Option<ReasoningEffort> {
    LEVELS
        .into_iter()
        .find(|level| effort_value(*level) == value)
}

/// One row before the current-marker is applied.
struct Row {
    value: &'static str,
    hint: String,
}

/// The row that clears, first — it is the state an agent starts in.
fn rows(t: &Translations) -> Vec<Row> {
    let mut rows = vec![Row {
        value: DEFAULT_LEVEL,
        hint: t.t(keys::menu::efforts::DEFAULT),
    }];
    rows.extend(LEVELS.into_iter().map(|level| Row {
        value: effort_value(level),
        hint: if level == ReasoningEffort::Off {
            t.t(keys::menu::efforts::OFF)
        } else {
            String::new()
        },
    }));
    rows
}

/// Which row is in force. Stating none is `default`, which is a real answer.
fn current_value(current: Option<ReasoningEffort>) -> &'static str {
    current.map_or(DEFAULT_LEVEL, effort_value)
}

/// One row per level, `default` first.
pub fn effort_items(current: Option<ReasoningEffort>, t: &Translations) -> Vec<SelectItem<String>> {
    let in_force = current_value(current);
    rows(t)
        .into_iter()
        .map(|row| {
            let hint = if row.value == in_force {
                with_current(&row.hint, t)
            } else {
                row.hint
            };
            SelectItem {
                value: row.value.to_owned(),
                label: row.value.to_owned(),
                hint: Some(hint),
                keywords: None,
                disabled: false,
            }
        })
        .collect()
}

/// The listing, for a terminal that cannot draw a menu.
pub fn effort_listing(current: Option<ReasoningEffort>, t: &Translations) -> String {
    let in_force = current_value(current);
    rows(t)
        .into_iter()
        .map(|row| {
            let mark = if row.value == in_force { '*' } else { ' ' };
            if row.hint.is_empty() {
                format!("{mark} {}", row.value)
            } else {
                format!("{mark} {}  ·  {}", row.value, row.hint)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Opens the menu on the level in force. `None` if it was cancelled.
pub async fn pick_effort(
    menu: &dyn PickerMenu,
    current: Option<ReasoningEffort>,
    t: &Translations,
) -> Option<String> {
    let items = effort_items(current, t);
    let at = position_of(&items, &current_value(current).to_owned());
    choose_from(
        menu,
        items,
        &t.t(keys::menu::titles::EFFORT),
        at,
        t,
        Placement::Prompt,
    )
    .await
}
