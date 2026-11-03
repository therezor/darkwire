//! Replays a stream of terminal bytes and prints the screen it produced.
//!
//! Not a test and not shipped: a way to answer "what would a terminal be
//! showing" for a recording of a real session, when the alternative is
//! squinting at escape sequences.

use std::io::Read;
use std::num::NonZeroU16;

fn main() -> std::io::Result<()> {
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes)?;
    let rows: u16 = std::env::var("REPLAY_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24);
    let columns: u16 = std::env::var("REPLAY_COLS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(92);
    let mut parser = vt100::Parser::new(at_least_one(rows), at_least_one(columns), 200);
    // `REPLAY_STEPS=offset:cols:rows,...` resizes the emulator part way
    // through, which is what a window being dragged does to a real one.
    let steps: Vec<(usize, u16, u16)> = std::env::var("REPLAY_STEPS")
        .unwrap_or_default()
        .split(',')
        .filter(|step| !step.is_empty())
        .filter_map(|step| {
            let mut parts = step.split(':');
            Some((
                parts.next()?.parse().ok()?,
                parts.next()?.parse().ok()?,
                parts.next()?.parse().ok()?,
            ))
        })
        .collect();
    let mut at = 0;
    for (offset, cols, rws) in steps {
        let offset = offset.min(bytes.len());
        parser.process(&bytes[at..offset]);
        parser
            .screen_mut()
            .set_size(at_least_one(rws), at_least_one(cols));
        at = offset;
    }
    parser.process(&bytes[at..]);
    let screen = parser.screen();
    let (rows, columns) = screen.size();
    println!("--- screen ---");
    for row in 0..rows.get() {
        println!(
            "{:2} |{}",
            row,
            screen.contents_between(row, 0, row, columns.get())
        );
    }
    let (row, column) = screen.cursor_position();
    println!("--- cursor at row {row}, column {column} ---");
    Ok(())
}

/// vt100 has no empty screen, so a zero side is the smallest one it has.
fn at_least_one(cells: u16) -> NonZeroU16 {
    NonZeroU16::new(cells).unwrap_or(NonZeroU16::MIN)
}
