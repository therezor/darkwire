//! The injected randomness seam.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_security::testkit::FixedRandom;
use darkwire_security::{OsRandom, RandomSource};

#[test]
fn the_os_source_fills_every_byte_and_does_not_repeat() {
    let mut first = [0u8; 32];
    let mut second = [0u8; 32];
    OsRandom.fill(&mut first);
    OsRandom.fill(&mut second);
    // Two 256-bit draws colliding means the source is not random, which would
    // make every envelope in the process forgeable.
    assert_ne!(first, second);
    assert!(first.iter().any(|b| *b != 0));
}

#[test]
fn the_os_source_handles_an_empty_buffer() {
    let mut empty: [u8; 0] = [];
    OsRandom.fill(&mut empty);
}

#[test]
fn the_fixed_source_repeats_a_constant() {
    let mut buf = [0u8; 5];
    FixedRandom::constant(0xab).fill(&mut buf);
    assert_eq!(buf, [0xab; 5]);
}

#[test]
fn the_fixed_source_cycles_a_pattern() {
    let mut buf = [0u8; 7];
    FixedRandom::pattern(&[1, 2, 3]).fill(&mut buf);
    assert_eq!(buf, [1, 2, 3, 1, 2, 3, 1]);
}

#[test]
fn the_fixed_source_with_no_pattern_fills_zeros() {
    let mut buf = [9u8; 3];
    FixedRandom::pattern(&[]).fill(&mut buf);
    assert_eq!(buf, [0, 0, 0]);
}
