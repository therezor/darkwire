//! Staying inside the result budget, and cutting where a reader expects.

use darkwire_tools::web::{MIN_EXTRACT, RESERVE, cut_at_line, share, width};

#[test]
fn length_is_measured_in_the_unit_the_registry_counts() {
    // Bytes would let a CJK page overshoot by three to one, which is the whole
    // reason this is not `len()`.
    assert_eq!(width("abc"), 3);
    assert_eq!(width("日本語"), 3);
    assert_eq!("日本語".len(), 9);
}

#[test]
fn text_inside_the_limit_is_returned_whole() {
    let (head, dropped) = cut_at_line("one\ntwo", 100);
    assert_eq!(head, "one\ntwo");
    assert_eq!(dropped, 0);
}

#[test]
fn a_zero_limit_means_no_limit() {
    let (head, dropped) = cut_at_line("one\ntwo", 0);
    assert_eq!(head, "one\ntwo");
    assert_eq!(dropped, 0);
}

/// A cut mid-sentence reads as corruption; a cut at a newline reads as an
/// ending.
#[test]
fn a_cut_lands_on_a_line_boundary() {
    let text = "first line here\nsecond line here\nthird line here";
    let (head, dropped) = cut_at_line(text, 34);
    assert_eq!(head, "first line here\nsecond line here");
    assert!(dropped > 0);
    assert_eq!(dropped, width(text) - width(&head));
}

/// One enormous line must not come back empty, so the boundary is only honoured
/// past halfway.
#[test]
fn a_single_long_line_is_cut_rather_than_dropped() {
    let text = format!("short\n{}", "x".repeat(200));
    let (head, dropped) = cut_at_line(&text, 100);
    assert_eq!(width(&head), 100);
    assert!(dropped > 0);
}

#[test]
fn a_share_is_what_is_left_after_the_listing_and_the_reserve() {
    let (each, fits) = share(8192, 1000, 3);
    assert_eq!(fits, 3);
    assert_eq!(each, (8192 - RESERVE - 1000) / 3);
    assert!(each >= MIN_EXTRACT);
}

/// Fewer pages read properly beats six openings of six paragraphs, and beats
/// overflowing into the registry's head-and-tail cut.
#[test]
fn too_little_budget_reduces_the_reads_rather_than_the_share() {
    // 3000 less the reserve and the listing is 2400, which three ways is 800
    // and under the floor, so two extracts of 1200 is the answer.
    let (each, fits) = share(3000, 200, 3);
    assert_eq!(fits, 2);
    assert_eq!(each, 1200);
    assert!(each >= MIN_EXTRACT);

    // Enough room for all three keeps all three.
    let (each, fits) = share(8192, 200, 3);
    assert_eq!(fits, 3);
    assert!(each >= MIN_EXTRACT);
}

#[test]
fn a_budget_too_small_for_one_extract_supports_none() {
    assert_eq!(share(RESERVE + 100, 0, 3), (0, 0));
    assert_eq!(share(8192, 0, 0), (0, 0));
}
