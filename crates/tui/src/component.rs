//! Things that turn a width into rows.
//!
//! A component returns *drawn rows*, not prose: one entry is one row on screen,
//! folded to the width it was asked for. That single rule is what makes a
//! resize uneventful. Nothing here is placed by a coordinate, so a window that
//! changed size is answered by asking for the rows again.

/// A width in, rows out.
pub trait Component {
    /// The rows this component occupies at `width` columns.
    ///
    /// Takes `&mut self` so a component may cache what it drew — a transcript
    /// re-wrapping ten thousand lines on every keystroke would make typing cost
    /// the length of the conversation.
    fn render(&mut self, width: usize) -> Vec<String>;
}

/// Where the terminal's own cursor belongs, emitted inline by whichever
/// component owns it.
///
/// An APC string: terminals ignore it, it occupies no columns, and it travels
/// with the text around it — so a component says "here" in the middle of the
/// row it is drawing rather than reporting a coordinate that some other code
/// would have to keep in step with the drawing.
///
/// Terminated by ST (`ESC \`) rather than BEL, which is the standard and not a
/// preference. BEL closes an *OSC* string as an xterm extension; nothing says
/// it closes an APC one, and a terminal that waits for ST would swallow
/// everything written after the marker until one arrived.
/// [`crate::ansi::styled_line`] drops every marker on the way to a buffer, so
/// this should never reach a terminal at all — which is exactly why it should
/// be the form that is harmless if it ever does.
pub const CURSOR_MARKER: &str = "\x1b_darkwire:cursor\x1b\\";
