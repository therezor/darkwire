//! Where agent presets are found, and what counts as one.
//!
//! The TypeScript half had no test file of its own — the listing and the
//! parsing were covered only through `preset install`. They are covered
//! directly here, because the resolution order is the part an operator's own
//! preset depends on and "the catalogue's won" is a silent failure.

// `allow-unwrap-in-tests` covers a `#[test]` body; the fixture helpers beside
// them are ordinary functions, and a helper that returned `Result` would make
// every case carry a `?` for a temporary directory that cannot fail in a way
// the case could act on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "fixture helpers in an integration test"
)]

use std::path::{Path, PathBuf};

use ghostai::presets::{
    find_preset, list_all_presets, list_presets, parse_preset, preset_dirs, read_preset,
};
use ghostai_core::ErrorKind;
use tempfile::TempDir;

/// The smallest thing that parses.
fn preset_json(id: &str) -> String {
    format!(r#"{{"schema":"ghostai.agent-preset/1","id":"{id}"}}"#)
}

fn write_preset(dir: &Path, id: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("{id}.json"));
    std::fs::write(&path, preset_json(id)).unwrap();
    path
}

#[test]
fn searches_the_operators_directory_before_the_catalogues() {
    // What somebody put on this machine is more specific than what a package
    // guessed, so a local preset wins over the catalogue's of the same name.
    let root = TempDir::new().unwrap();
    let mine = root.path().join("presets");
    let catalogue = root.path().join("agents");
    let local = write_preset(&mine, "coder");
    write_preset(&catalogue, "coder");

    let dirs = preset_dirs(&mine, Some(&catalogue));

    assert_eq!(dirs, vec![mine.clone(), catalogue]);
    assert_eq!(find_preset(&dirs, "coder"), Some(local));
}

#[test]
fn is_one_directory_when_there_is_no_catalogue() {
    let root = TempDir::new().unwrap();
    let mine = root.path().join("presets");

    assert_eq!(preset_dirs(&mine, None), vec![mine]);
}

#[test]
fn answers_with_nothing_for_a_name_no_directory_holds() {
    let root = TempDir::new().unwrap();
    let mine = root.path().join("presets");
    write_preset(&mine, "coder");

    assert_eq!(find_preset(&preset_dirs(&mine, None), "nowhere"), None);
}

#[test]
fn lists_the_stems_of_the_json_files_sorted() {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("presets");
    write_preset(&dir, "writer");
    write_preset(&dir, "coder");
    // Neither a preset nor an id: the suffix is what makes a file one.
    std::fs::write(dir.join("README.md"), "not a preset").unwrap();

    assert_eq!(list_presets(&dir), vec!["coder", "writer"]);
}

#[test]
fn treats_a_missing_directory_as_empty() {
    // `<root>/presets` exists only once somebody has put something in it, so a
    // listing has to answer for a directory that was never created.
    let root = TempDir::new().unwrap();

    assert!(list_presets(&root.path().join("never-made")).is_empty());
}

#[test]
fn deduplicates_across_directories() {
    let root = TempDir::new().unwrap();
    let mine = root.path().join("presets");
    let catalogue = root.path().join("agents");
    write_preset(&mine, "coder");
    write_preset(&catalogue, "coder");
    write_preset(&catalogue, "writer");

    let dirs = preset_dirs(&mine, Some(&catalogue));

    assert_eq!(list_all_presets(&dirs), vec!["coder", "writer"]);
}

#[test]
fn names_the_file_when_the_text_is_not_json() {
    let error = parse_preset("{ not json", "/tmp/coder.json").unwrap_err();

    assert_eq!(error.kind, ErrorKind::Config);
    assert!(
        error.message.contains("/tmp/coder.json"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("not valid JSON"),
        "{}",
        error.message
    );
}

#[test]
fn names_the_file_when_the_json_is_not_a_preset() {
    let error = parse_preset(r#"{"id":"coder"}"#, "/tmp/coder.json").unwrap_err();

    assert_eq!(error.kind, ErrorKind::Config);
    assert!(
        error.message.contains("is not a valid agent preset"),
        "{}",
        error.message
    );
}

#[test]
fn refuses_an_id_the_schema_would_refuse() {
    // The parser holds a preset to the schema's own constraints and no others.
    // An id of the wrong *shape* — `Not An Id` — parses here and is refused by
    // `ghostai agent install`, which is where the rule belongs: it is the same
    // rule a settings save applies, and the message it writes names the command
    // that was run. `tests/agent.rs` covers that half.
    assert!(parse_preset(&preset_json("Not An Id"), "/tmp/x.json").is_ok());

    let too_long = "a".repeat(41);
    let error = parse_preset(&preset_json(&too_long), "/tmp/x.json").unwrap_err();

    assert!(error.message.contains("id:"), "{}", error.message);
}

#[test]
fn refuses_a_skill_name_that_is_not_a_directory_name() {
    // Every entry becomes a path segment. A name that is not a slug is a
    // traversal waiting for a join, so it never reaches the copier.
    let text = r#"{"schema":"ghostai.agent-preset/1","id":"coder","skills":["../escape"]}"#;

    let error = parse_preset(text, "/tmp/coder.json").unwrap_err();

    // The validator's own path spelling, not a second one written here: the
    // issue list is what an operator reads, and a path this file invented would
    // be a path nothing else in the tree uses.
    assert!(error.message.contains("skills[0]"), "{}", error.message);
}

#[test]
fn accepts_a_preset_that_names_only_what_it_must() {
    let preset = parse_preset(&preset_json("coder"), "/tmp/coder.json").unwrap();

    assert_eq!(preset.id, "coder");
    assert!(preset.skills.is_empty());
}

#[test]
fn reads_one_from_disk_and_names_the_path_when_it_cannot() {
    let root = TempDir::new().unwrap();
    let path = write_preset(&root.path().join("presets"), "coder");

    assert_eq!(read_preset(&path).unwrap().id, "coder");

    let missing = root.path().join("presets").join("nowhere.json");
    let error = read_preset(&missing).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(
        error.message.contains("could not be read"),
        "{}",
        error.message
    );
}
