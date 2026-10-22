//! The message shapes' own arithmetic.

use darkwire_protocol::Calibration;

fn calibration(reported_tokens: u64, estimated_tokens: u64) -> Calibration {
    Calibration {
        reported_tokens,
        estimated_tokens,
        model: "m".to_owned(),
    }
}

#[test]
fn a_ratio_is_reported_over_estimated() {
    assert!((calibration(150, 100).ratio() - 1.5).abs() < f64::EPSILON);
}

#[test]
fn a_zero_estimate_still_gives_a_finite_ratio() {
    // Validation guards the wire, not a row read back from storage.
    assert!(calibration(500, 0).ratio().is_finite());
    assert!(calibration(0, 0).ratio().is_finite());
}
