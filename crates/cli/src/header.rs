//! What the REPL says about itself: once at the top, and once under every
//! prompt.
//!
//! Both are functions over one plain record — no runtime, no store, no session
//! — which is what makes them assertable without booting anything, and why the
//! caller does the looking-up.
//!
//! **The frame has a rule above the editor and one below, and they are drawn by
//! different things.** The one below is part of the status bar, under the
//! cursor, where a prompt string cannot reach. The one above *is* the prompt —
//! the only part of the frame written into the scrollback — so the caller takes
//! the whole prompt block down again when a turn starts and prints the message
//! itself. That is why the rule is a function here rather than a constant in
//! the prompt: both halves are the same width, measured the same way.
//!
//! The two rows are laid out with [`justify`], so the fields that change — the
//! context budget and the model — are the ones anchored to the right edge and
//! the workspace name is what gets truncated when the window is narrow.

use darkwire_i18n::keys;
use darkwire_tui::{Theme, justify, pad_to_width, rule, truncate_to_width, visible_width};

use crate::i18n::Translations;

/// The glyph both rules are drawn with.
///
/// Named rather than typed at each of the two call sites: the rule above the
/// editor and the one below it have to be the same line, and a second literal
/// is a second thing to change.
pub const RULE_GLYPH: &str = "─";

/// What a cut label is marked with when the window is too narrow for it.
const ELLIPSIS: &str = "…";

/// How much of the model's window the next turn would fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextUsage {
    /// What the next request would carry.
    pub used_tokens: u64,
    /// The model's budget.
    pub window_tokens: u64,
}

/// Everything the header and the status bar draw, already looked up.
///
/// A plain record, so both functions below are assertable without a runtime, a
/// store or a session — and so the looking-up happens once, in the caller that
/// already holds all three.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderView {
    /// The agent's label, or its id when it has no label of its own.
    pub agent: String,
    /// The model this session's agent runs.
    pub model: String,
    /// The provider instance behind that model. Empty when there is none.
    pub provider: String,
    /// The folder the workspaces live in, which is what the startup header
    /// shows. Not one workspace's directory: resolving that would create it as
    /// a side effect of printing a line.
    pub workspaces: String,
    /// The workspace's name in the registry, which is what the bar shows.
    pub workspace_name: String,
    /// The conversation's title, falling back to its key.
    pub session: String,
    /// Absent until a turn has run and there is something to measure.
    pub context: Option<ContextUsage>,
}

/// The five labelled rows, in the order they are printed.
fn rows_for(view: &HeaderView) -> [(&'static str, &str); 5] {
    [
        (keys::chat::header::AGENT, view.agent.as_str()),
        (keys::chat::header::MODEL, view.model.as_str()),
        (keys::chat::header::PROVIDER, view.provider.as_str()),
        (keys::chat::header::WORKSPACES, view.workspaces.as_str()),
        (keys::chat::header::SESSION, view.session.as_str()),
    ]
}

/// The block printed once, when the prompt opens.
///
/// The label column is measured rather than typed out: a run of hand-counted
/// spaces holds only until the first translation is longer than the English it
/// replaced, and then it is wrong for every row at once.
///
/// `shortcuts` is whether this terminal can draw a menu, which decides between
/// the two hints — offering `ctrl-g` on a pipe would be advertising a key that
/// does nothing.
#[must_use]
pub fn startup_header(
    view: &HeaderView,
    width: usize,
    theme: &Theme,
    t: &Translations,
    shortcuts: bool,
) -> String {
    let labels: Vec<String> = rows_for(view).iter().map(|(key, _)| t.t(key)).collect();
    let column = labels
        .iter()
        .map(|label| visible_width(label))
        .max()
        .unwrap_or(0);

    let mut lines = Vec::with_capacity(labels.len() + 4);
    lines.push(theme.title.apply(&theme.accent.apply("  darkwire")));
    for (label, (_, value)) in labels.iter().zip(rows_for(view)) {
        let padded = theme.dim.apply(&pad_to_width(label, column));
        lines.push(truncate_to_width(
            &format!("  {padded}  {value}"),
            width,
            ELLIPSIS,
        ));
    }
    lines.push(String::new());

    let hint = if shortcuts {
        t.t(keys::chat::header::HINT_MENU)
    } else {
        t.t(keys::chat::header::HINT)
    };
    lines.push(truncate_to_width(
        &format!("  {}", theme.dim.apply(&hint)),
        width,
        ELLIPSIS,
    ));
    // A trailing blank, which the frame's own gap above the editor then
    // doubles: the welcome is a block about the install rather than part of the
    // conversation, and one line of gap reads as though it were the first
    // message.
    lines.push(String::new());

    lines.join("\n")
}

/// `12.4%/128k`, or nothing at all before a turn has been measured.
///
/// A window of nothing answers nothing rather than dividing by it: an
/// unconfigured agent has no budget, and `NaN%` on the status bar is worse than
/// a blank.
#[must_use]
pub fn context_label(context: Option<&ContextUsage>) -> String {
    let Some(context) = context.filter(|usage| usage.window_tokens > 0) else {
        return String::new();
    };
    // Token counts are far below 2^53, where an f64 is still exact.
    #[allow(clippy::cast_precision_loss)]
    let percent = (context.used_tokens as f64 / context.window_tokens as f64) * 100.0;
    // Nearest thousand, rounding halves up, without going through a float: a
    // budget is an integer and the label is read at a glance.
    let window = context.window_tokens.saturating_add(500) / 1000;
    format!("{percent:.1}%/{window}k")
}

/// The rule and the two status rows that sit under the editor.
///
/// Returned as lines rather than a string because the bottom bar addresses one
/// row at a time, and because the height is what the caller has to reserve
/// before the prompt is drawn.
#[must_use]
pub fn status_bar(view: &HeaderView, width: usize, theme: &Theme) -> Vec<String> {
    let context = context_label(view.context.as_ref());
    let model = if view.provider.is_empty() {
        view.model.clone()
    } else {
        format!("{}/{}", view.provider, view.model)
    };

    vec![
        theme.dim.apply(&rule(width, RULE_GLYPH)),
        theme
            .dim
            .apply(&justify(&view.workspace_name, &view.agent, width)),
        theme.dim.apply(&justify(&context, &model, width)),
    ]
}

/// The rule above the editor, which the prompt string carries.
///
/// One column short of the window, like the rows below it: writing exactly
/// `columns` characters leaves a terminal in a pending-wrap state that
/// emulators resolve differently, and everything here is followed by cursor
/// motion that assumes it knows which row it is on.
#[must_use]
pub fn input_rule(width: usize, theme: &Theme) -> String {
    theme.dim.apply(&rule(width, RULE_GLYPH))
}
