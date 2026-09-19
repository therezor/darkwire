//! A terminal toolkit that has never heard of an agent.
//!
//! This crate knows four things: how to turn bytes from a terminal into keys,
//! how wide a string is when a terminal draws it, how to hold a line somebody
//! is typing, and how to keep a frame of rows on screen and correct. It knows
//! nothing about sessions, models, configuration or translation — every string
//! that reaches it is prose the caller has already translated, which is why
//! [`SelectItem::label`] is a `String` and not a key.
//!
//! That boundary is not a convention. `Cargo.toml` here declares no workspace
//! dependency at all, so a `use` of one does not resolve: the layering is a
//! fact about the crate graph rather than a rule a reviewer has to remember.
//! It sits beside the roots of the graph, not under them, and only the
//! `darkwire` binary reaches for it.
//!
//! Components render rows for a width, and one renderer owns the whole frame
//! and redraws it whole when the window changes size. A terminal rewraps its
//! own screen before the program hears about it; [`renderer`] has the
//! measurement that decided that.
#![forbid(unsafe_code)]

pub mod block;
pub mod component;
pub mod editor;
pub mod keys;
pub mod pages;
pub mod renderer;
pub mod select;
pub mod select_list;
pub mod spinner;
pub mod terminal;
pub mod text;
pub mod theme;
pub mod transcript;

pub use block::Block;
pub use component::{CURSOR_MARKER, Component};
pub use editor::{Editor, EditorOutcome};
pub use keys::{Key, KeyName, is_ctrl, parse_key, parse_keys};
pub use pages::{
    CHROME_ROWS as PAGES_CHROME_ROWS, Page, Pages, PagesLabels, PagesOptions, PagesOutcome,
};
pub use renderer::{FRAME_INTERVAL_MS, Renderer, RendererOptions};
pub use select::{
    CHROME_ROWS, DEFAULT_MAX_ROWS, Select, SelectLabels, SelectOptions, SelectOutcome,
};
pub use select_list::{SelectItem, SelectList};
pub use spinner::{SPINNER_FRAMES, SPINNER_INTERVAL_MS, spinner_frame};
pub use terminal::{
    Keyboard, StandardInput, StandardOutput, TerminalInput, TerminalOutput, columns_of,
    open_keyboard, rows_of,
};
pub use text::{
    STYLE_RESET, carry_styles, drop_last_grapheme, fit_to_width, justify, next_boundary,
    pad_to_width, previous_boundary, rule, strip_ansi, truncate_start_to_width, truncate_to_width,
    visible_width, wrap_to_width,
};
pub use theme::{PLAIN_THEME, Palette, Style, Theme, palette_for, theme_for, theme_from};
pub use transcript::Transcript;
