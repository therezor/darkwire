//! Folding a styled line into the rows a width can hold.
//!
//! [`crate::text::wrap_to_width`] does this for a string with SGR escapes in
//! it, where a style is a sequence in the middle of the text and carrying one
//! across a fold means re-opening it. A [`Line`] has no such problem: a style
//! belongs to a span, so a fold is arithmetic on spans and nothing has to be
//! re-opened. The rules are the same ones a reader expects either way — break
//! at a space when there is one, break mid-cluster when there is not, and let
//! a cluster wider than the whole window overhang rather than loop.
//!
//! This is what the scrollback writer folds with, so its answer is what the
//! terminal keeps. A row that came back too wide would be folded again by the
//! emulator, and the program's idea of how many rows it wrote would be short.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// The leading whitespace of a line, styled as it was found.
///
/// A wrapped list item or quoted block reads as one thing when its folded rows
/// start under the first one's text rather than under its bullet.
#[must_use]
pub fn leading_whitespace(line: &Line<'_>) -> String {
    let mut prefix = String::new();
    for span in &line.spans {
        let end = span
            .content
            .char_indices()
            .find_map(|(index, ch)| (!ch.is_whitespace()).then_some(index))
            .unwrap_or(span.content.len());
        prefix.push_str(&span.content[..end]);
        if end < span.content.len() {
            break;
        }
    }
    prefix
}

/// One logical line as the rows `width` can hold, with an indent on the rest.
///
/// The first row starts where the line does; every row after it starts with
/// `indent`, which is ordinarily the line's own leading whitespace.
#[must_use]
pub fn wrap_line(line: &Line<'static>, width: u16, indent: &str) -> Vec<Line<'static>> {
    let width = usize::from(width);
    if width == 0 {
        return vec![line.clone()];
    }
    if line_width(line) <= width {
        return vec![line.clone()];
    }

    let mut folder = Folder::new(width, indent, line.style);
    for span in &line.spans {
        folder.push_span(span);
    }
    folder.finish()
}

/// How many columns the line occupies when drawn.
#[must_use]
pub fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| span.content.as_ref().width())
        .sum()
}

/// A line's rows folded and then counted, without keeping them.
#[must_use]
pub fn wrapped_height(line: &Line<'static>, width: u16) -> u16 {
    let indent = leading_whitespace(line);
    let rows = wrap_line(line, width, &indent).len();
    u16::try_from(rows).unwrap_or(u16::MAX)
}

/// Every line folded in turn, in order.
#[must_use]
pub fn wrap_lines(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let indent = leading_whitespace(line);
        out.extend(wrap_line(line, width, &indent));
    }
    out
}

/// The state of one line being folded.
///
/// The row under construction is a cluster at a time, each with the style it
/// arrived in, so a fold point is an index and nothing has to be recomputed.
/// Adjacent clusters of one style become one span when the row is finished,
/// which is the only place spans are built.
struct Folder<'a> {
    width: usize,
    indent: &'a str,
    line_style: Style,
    rows: Vec<Line<'static>>,
    row: Vec<(String, Style)>,
    used: usize,
    /// Where in `row` the last space sits, when the row has one.
    break_at: Option<usize>,
}

impl<'a> Folder<'a> {
    fn new(width: usize, indent: &'a str, line_style: Style) -> Self {
        Self {
            width,
            indent,
            line_style,
            rows: Vec::new(),
            row: Vec::new(),
            used: 0,
            break_at: None,
        }
    }

    fn push_span(&mut self, span: &Span<'static>) {
        for cluster in span.content.graphemes(true) {
            self.place(cluster, span.style);
        }
    }

    fn place(&mut self, cluster: &str, style: Style) {
        // A space at the end of a row is allowed to overhang, because a fold
        // is about to drop it. Folding *on* it instead would end the row one
        // word early: "one two" at a width of seven would fold after "one".
        if cluster == " " {
            if self.used > 0 {
                self.break_at = Some(self.row.len());
            }
            self.row.push((cluster.to_owned(), style));
            self.used += 1;
            return;
        }

        let cost = cluster.width();
        // `used > 0` keeps a cluster wider than the window from folding for
        // ever onto empty rows: it goes on the row and overhangs by one.
        if self.used + cost > self.width && self.used > 0 {
            self.fold();
        }
        self.row.push((cluster.to_owned(), style));
        self.used += cost;
    }

    /// Ends the row, carrying whatever followed the last space onto the next.
    fn fold(&mut self) {
        let carried = match self.break_at.take() {
            // The spaces are what the fold replaces, so they go: the clusters
            // before them end the row and the ones after start the next. All
            // of them, not only the one the break was recorded at — a run of
            // spaces left on the end is a row wider than the window it was
            // folded for, and the terminal would fold it again.
            Some(space) => {
                let after = self.row.split_off(space);
                self.trim_trailing_spaces();
                after
                    .into_iter()
                    .skip_while(|(cluster, _)| cluster == " ")
                    .collect()
            }
            None => Vec::new(),
        };

        let finished = std::mem::take(&mut self.row);
        self.rows.push(self.spans_to_line(finished));

        // Every row after the first starts under the first one's text.
        self.row = Vec::new();
        self.used = 0;
        if !self.indent.is_empty() {
            for cluster in self.indent.graphemes(true) {
                self.used += cluster.width();
                self.row.push((cluster.to_owned(), Style::default()));
            }
        }
        for (cluster, style) in carried {
            self.used += cluster.as_str().width();
            self.row.push((cluster, style));
        }
    }

    /// Drops the spaces a row ends in.
    ///
    /// They are invisible and they count: a row that is `width` columns of
    /// text and thirty of trailing space is a row the terminal folds in two,
    /// and everything measured from this side is then short by one.
    fn trim_trailing_spaces(&mut self) {
        while self
            .row
            .last()
            .is_some_and(|(cluster, _)| cluster.as_str() == " ")
        {
            self.row.pop();
        }
    }

    /// Runs of one style become one span, which is what a reader of the line
    /// expects and what keeps the scrollback writer from emitting an escape
    /// sequence per character.
    fn spans_to_line(&self, clusters: Vec<(String, Style)>) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (cluster, style) in clusters {
            match spans.last_mut() {
                Some(last) if last.style == style => last.content.to_mut().push_str(&cluster),
                _ => spans.push(Span::styled(cluster, style)),
            }
        }
        Line::from(spans).style(self.line_style)
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        // A last row that ran past the width on spaces alone would be folded
        // by the terminal the same way any other over-wide row is.
        if self.used > self.width {
            self.trim_trailing_spaces();
        }
        let finished = std::mem::take(&mut self.row);
        self.rows.push(self.spans_to_line(finished));
        self.rows
    }
}
