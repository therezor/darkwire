//! `fixtures/merge/patches.json`, replayed as the chain it was recorded from.
//!
//! The file is written by the TypeScript suite that owns the behaviour, and this
//! runs every case through the Rust port. Two things about how it is read are
//! not obvious and both matter:
//!
//!  - **A successful case's `input.base` is its own `output`.** The emitter
//!    records the step after advancing, so feeding `input.base` back in would
//!    only prove the merge is idempotent — which a merge that returned its base
//!    unchanged also satisfies. So the chain is replayed instead: a case whose
//!    name has no step suffix, or ends in `(step 1)`, starts from the defaults,
//!    and every later step merges onto the previous step's result.
//!  - **The two error cases are compared on kind and path, not on wording.**
//!    The messages the emitter recorded are zod's ("Invalid input: expected
//!    string, received null"), and no Rust deserialiser produces that sentence.
//!    What the port owes is the same *refusal*, of the same kind, naming the
//!    same place; the prose belongs to whichever library rejected it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_core::ErrorKind;
use darkwire_protocol::Config;
use darkwire_runtime::merge_config_patch;
use serde_json::Value;

const PATCHES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/merge/patches.json"
));

/// Whether this case opens a new chain rather than continuing one.
fn starts_a_chain(name: &str) -> bool {
    !name.contains("(step ") || name.ends_with("(step 1)")
}

#[test]
fn matches_the_merge_fixture() {
    let fixture: Value = serde_json::from_str(PATCHES).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 38);

    let mut base = Config::default();
    let mut refusals = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        if starts_a_chain(name) {
            base = Config::default();
        }
        let patch = &case["input"]["patch"];
        let expected = &case["output"];

        match merge_config_patch(&base, patch) {
            Ok(next) => {
                let wanted: Config = serde_json::from_value(expected.clone())
                    .unwrap_or_else(|error| panic!("case {name}: output is not a Config: {error}"));
                assert_eq!(next, wanted, "case: {name}");
                // Compared as trees as well, so a field the Rust type happens to
                // default the same way cannot hide a difference in what was
                // written.
                assert_eq!(
                    serde_json::to_value(&next).unwrap(),
                    serde_json::to_value(&wanted).unwrap(),
                    "case: {name}"
                );
                base = next;
            }
            Err(error) => {
                refusals += 1;
                let recorded = &expected["error"];
                assert!(
                    recorded.is_object(),
                    "case {name}: expected a merge, got a refusal"
                );
                assert_eq!(recorded["kind"], error.kind.as_str(), "case: {name}");
                assert_eq!(error.kind, ErrorKind::Config, "case: {name}");
                // The prefix is the port's own, and every issue line names a
                // path. The wording after the path is the deserialiser's.
                let recorded_message = recorded["message"].as_str().unwrap();
                let prefix = "Settings patch produces invalid settings:";
                assert!(recorded_message.starts_with(prefix), "case: {name}");
                assert!(error.message.starts_with(prefix), "case: {name}");
                for path in paths_of(recorded_message) {
                    assert!(
                        error.message.contains(path.as_str())
                            // A flattened block reports at the entry rather than
                            // the field it buffered; the entry is still named.
                            || path
                                .rsplit_once('.')
                                .is_some_and(|(head, _)| error.message.contains(head)),
                        "case {name}: no issue named {path}\n{}",
                        error.message
                    );
                }
            }
        }
    }
    assert_eq!(refusals, 2, "the fixture records exactly two refusals");
}

/// The dotted path each issue line names.
fn paths_of(message: &str) -> Vec<String> {
    message
        .lines()
        .skip(1)
        .filter_map(|line| {
            line.trim()
                .split_once(": ")
                .map(|(path, _)| path.to_owned())
        })
        .collect()
}
