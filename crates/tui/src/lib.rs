//! A terminal toolkit that has never heard of an agent.
//!
//! This crate knows four things: how to drive a terminal, what a keypress is
//! called, how wide a string is when a terminal draws it, and how to hold a
//! line somebody is typing. It knows nothing about sessions,
//! models, configuration or translation — every string that reaches it is prose
//! the caller has already translated, which is why [`SelectItem::label`] is a
//! `String` and not a key.
//!
//! That boundary is not a convention. `Cargo.toml` here declares no workspace
//! dependency at all, so a `use` of one does not resolve: the layering is a
//! fact about the crate graph rather than a rule a reviewer has to remember.
//! It sits beside the roots of the graph, not under them, and only the
//! `darkwire` binary reaches for it.
//!
//! **This crate owns the terminal.** [`tui::Tui`] takes raw mode, holds the
//! live area at the bottom of the screen, writes finished rows above it into
//! the terminal's own scrollback, and hands out one stream of the things a
//! loop has to react to. Nothing above it writes a byte to a tty.
//!
//! The split inside is between *finished* and *live*. A [`HistoryCell`] is
//! finished: it is asked for its rows at a width, they go out once through
//! [`insert_history`], and they belong to the emulator after that — its
//! scrolling, its selection, its search. A [`Renderable`] is live: it is
//! redrawn every frame and written nowhere. Everything else here serves one
//! side or the other.
//!
//! A row is still sometimes a `String` with SGR escapes in it, which is a
//! bridge rather than a destination — see [`ansi`] for what it costs and what
//! replaces it.
#![forbid(unsafe_code)]

pub mod ansi;
pub mod component;
pub mod editor;
pub mod frame;
pub mod history_cell;
pub mod insert_history;
pub mod keys;
pub mod pages;
pub mod renderable;
pub mod select;
pub mod select_list;
pub mod spinner;
pub mod terminal;
#[cfg(feature = "testkit")]
pub mod testkit;
pub mod text;
pub mod theme;
pub mod tui;
pub mod wrap;

pub use ansi::{cursor_in, styled_line};
pub use component::{CURSOR_MARKER, Component};
pub use editor::{Editor, EditorOutcome, is_newline};
pub use history_cell::{HistoryCell, PlainCell};
pub use keys::{Key, KeyName, is_ctrl};
pub use pages::{
    CHROME_ROWS as PAGES_CHROME_ROWS, Page, Pages, PagesLabels, PagesOptions, PagesOutcome,
};
pub use renderable::{Column, Renderable, StyledRows, Wrapped};
pub use select::{
    CHROME_ROWS, DEFAULT_MAX_ROWS, Select, SelectAction, SelectLabels, SelectOptions, SelectOutcome,
};
pub use select_list::{SelectItem, SelectList};
pub use spinner::{SPINNER_FRAMES, SPINNER_INTERVAL_MS, spinner_frame};
pub use text::{
    STYLE_RESET, TAB_WIDTH, carry_styles, drop_last_grapheme, expand_controls, fit_to_width,
    justify, next_boundary, pad_to_width, previous_boundary, rule, strip_ansi,
    truncate_start_to_width, truncate_to_width, visible_width, wrap_to_width,
};
pub use theme::{PLAIN_THEME, Palette, Style, Theme, palette_for, theme_for, theme_from};
pub use wrap::{leading_whitespace, line_width, wrap_line, wrap_lines, wrapped_height};
