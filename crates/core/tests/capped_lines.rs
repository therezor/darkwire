//! Lines from a pipe, each held to a byte cap.

use darkwire_core::{CappedLine, CappedLines};

fn whole(text: &str) -> CappedLine {
    CappedLine {
        text: text.to_owned(),
        truncated: false,
    }
}

async fn read_all(input: &[u8], cap: usize) -> Vec<CappedLine> {
    let mut lines = CappedLines::new(input, cap);
    let mut out = Vec::new();
    while let Some(line) = lines.next_line().await.unwrap() {
        out.push(line);
    }
    out
}

#[tokio::test]
async fn reads_ordinary_lines_without_their_terminators() {
    assert_eq!(
        read_all(b"one\ntwo\r\n\nlast", 64).await,
        vec![whole("one"), whole("two"), whole(""), whole("last")]
    );
}

#[tokio::test]
async fn a_line_with_no_newline_keeps_only_the_cap_and_the_next_line_still_parses() {
    // A spinner: carriage returns and no newline, far past the cap.
    let mut input = b"\rspinning".repeat(10_000);
    input.extend_from_slice(b"\nok\n");
    let lines = read_all(&input, 64).await;
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text.len(), 64);
    assert!(lines[0].truncated);
    assert_eq!(lines[1], whole("ok"));
}

#[tokio::test]
async fn bytes_that_are_not_utf8_cost_a_character_not_the_reader() {
    let lines = read_all(b"a\xffb\nnext\n", 64).await;
    assert_eq!(lines[0].text, "a\u{fffd}b");
    assert_eq!(lines[1], whole("next"));
}

#[tokio::test]
async fn a_crlf_line_of_exactly_the_cap_is_whole() {
    assert_eq!(
        read_all(b"abc\r\nnext\r\n", 3).await,
        vec![
            whole("abc"),
            CappedLine {
                text: "nex".to_owned(),
                truncated: true,
            }
        ]
    );
}
