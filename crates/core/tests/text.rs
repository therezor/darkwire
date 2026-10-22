//! Cutting text to a byte budget.

use darkwire_core::text::truncate_bytes;

#[test]
fn text_within_the_budget_is_unchanged() {
    assert_eq!(truncate_bytes("hello", 5), "hello");
    assert_eq!(truncate_bytes("hello", 50), "hello");
}

#[test]
fn a_cut_backs_up_to_the_start_of_the_character_it_landed_in() {
    // "é" is two bytes, so a budget of two keeps only "a".
    assert_eq!(truncate_bytes("aé", 2), "a");
    assert_eq!(truncate_bytes("aé", 3), "aé");
    assert_eq!(truncate_bytes("é", 0), "");
}
