//! What the renderer draws: things that turn a width into rows.
//!
//! A component returns *drawn rows*, not prose — one entry is one row on
//! screen, and nothing it returns may be wide enough for the terminal to wrap
//! it itself. That single rule is what the renderer's arithmetic rests on, and
//! it is also the reason a resize is survivable at all: a frame that never
//! depends on the terminal to fold anything can simply be asked for again at
//! the new width.

/// A width in, rows out.
pub trait Component {
    /// The rows this component occupies at `width` columns.
    ///
    /// Takes `&mut self` so a component may cache what it drew — a transcript
    /// re-wrapping ten thousand lines on every keystroke would make typing cost
    /// the length of the conversation.
    fn render(&mut self, width: usize) -> Vec<String>;
}

/// A fixed frame: the rows are drawn as they are, whatever the width.
///
/// What a test hands the renderer, and what a caller with pre-wrapped rows
/// hands it. The renderer cuts anything wider than the window on the way out.
impl Component for Vec<String> {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.clone()
    }
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
/// everything written after the marker until one arrived. The renderer strips
/// every marker before a frame is written, so this should never reach a
/// terminal at all — which is exactly why it should be the form that is
/// harmless if it ever does.
pub const CURSOR_MARKER: &str = "\x1b_darkwire:cursor\x1b\\";
