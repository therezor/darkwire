//! `ghostai extension` and `ghostai toolbox` — the two approval surfaces.
//!
//! One file for both, because they are one command written twice: the same
//! three verbs, the same exit codes, the same rule that an approval is a
//! statement about specific bytes rather than a flag. What differs is what
//! there is to review before approving, and that is what the listing cases are
//! about.

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

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ghostai::i18n::Env;
use ghostai::program::{Globals, StoreAction};
use ghostai::{Streams, extension, toolbox};

/// A pair of buffers a command writes into, read back as text.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Sink {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Run {
    code: u8,
    out: String,
    err: String,
}

fn globals(home: &Path) -> Globals {
    Globals {
        home: Some(home.display().to_string()),
        ..Globals::default()
    }
}

fn run_toolbox(home: &Path, action: StoreAction, id: Option<&str>) -> Run {
    let out = Sink::default();
    let err = Sink::default();
    let mut streams = Streams {
        out: Box::new(out.clone()),
        err: Box::new(err.clone()),
    };
    let code = toolbox::run(&globals(home), action, id, &Env::empty(), &mut streams).unwrap();
    Run {
        code,
        out: out.text(),
        err: err.text(),
    }
}

fn run_extension(home: &Path, action: StoreAction, id: Option<&str>) -> Run {
    let out = Sink::default();
    let err = Sink::default();
    let mut streams = Streams {
        out: Box::new(out.clone()),
        err: Box::new(err.clone()),
    };
    let code = extension::run(&globals(home), action, id, &Env::empty(), &mut streams).unwrap();
    Run {
        code,
        out: out.text(),
        err: err.text(),
    }
}

/// A digest-pinned image, which the security layer requires.
///
/// A tag can be repointed after an approval, which would leave the recorded
/// hash matching an image nobody reviewed.
const IMAGE: &str =
    "ghcr.io/example/box@sha256:0000000000000000000000000000000000000000000000000000000000000001";

/// The same image with one byte moved, for the case that proves an edit revokes.
const OTHER_IMAGE: &str =
    "ghcr.io/example/box@sha256:0000000000000000000000000000000000000000000000000000000000000002";

/// A toolbox manifest on disk, installed but not approved.
fn install_toolbox(home: &Path, name: &str) {
    let dir = home.join("toolboxes").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("toolbox.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "ghostai.toolbox/1",
            "name": name,
            "image": IMAGE,
        }))
        .unwrap(),
    )
    .unwrap();
}

/// An extension directory on disk, installed but not approved.
fn install_extension(home: &Path, id: &str) {
    let dir = home.join("extensions").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ghostai.extension.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "ghostai.extension/2",
            "id": id,
            "version": "1.2.3",
            "description": "an example",
            "command": ["node", "index.mjs"],
            "contributes": ["tools"],
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("index.mjs"), "// nothing").unwrap();
}

// toolbox

#[test]
fn toolbox_list_says_where_it_looked_when_nothing_is_installed() {
    let home = tempfile::tempdir().unwrap();
    let run = run_toolbox(home.path(), StoreAction::List, None);
    assert_eq!(run.code, 0);
    assert!(
        run.out.contains("No toolboxes installed under"),
        "{}",
        run.out
    );
    // The directory itself, so an operator who installed one somewhere else can
    // see where this build is looking.
    assert!(run.out.contains("toolboxes"), "{}", run.out);
}

#[test]
fn toolbox_list_shows_what_an_operator_has_to_weigh() {
    // A review that shows only a name is a rubber stamp with extra steps.
    let home = tempfile::tempdir().unwrap();
    install_toolbox(home.path(), "sandbox");
    let run = run_toolbox(home.path(), StoreAction::List, None);

    assert_eq!(run.code, 0);
    assert!(run.out.contains("sandbox"), "{}", run.out);
    assert!(run.out.contains("NOT APPROVED"), "{}", run.out);
    assert!(run.out.contains("image"), "{}", run.out);
    assert!(run.out.contains("network"), "{}", run.out);
    assert!(run.out.contains("limits"), "{}", run.out);
}

#[test]
fn toolbox_approve_records_the_manifest_hash_and_says_what_that_means() {
    // Not a flag being set: a statement about specific content. Editing the
    // manifest afterwards changes the hash and revokes the approval, which is
    // why nothing here needs a `--force`.
    let home = tempfile::tempdir().unwrap();
    install_toolbox(home.path(), "sandbox");

    let approved = run_toolbox(home.path(), StoreAction::Approve, Some("sandbox"));
    assert_eq!(approved.code, 0);
    assert!(
        approved.out.contains("Approved sandbox"),
        "{}",
        approved.out
    );
    assert!(approved.out.contains("sha256:"), "{}", approved.out);
    assert!(
        approved.out.contains("revokes this approval"),
        "{}",
        approved.out
    );

    let listed = run_toolbox(home.path(), StoreAction::List, None);
    assert!(listed.out.contains("[approved]"), "{}", listed.out);
}

#[test]
fn editing_the_manifest_revokes_the_approval_by_itself() {
    let home = tempfile::tempdir().unwrap();
    install_toolbox(home.path(), "sandbox");
    run_toolbox(home.path(), StoreAction::Approve, Some("sandbox"));

    let manifest = home.path().join("toolboxes/sandbox/toolbox.json");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, text.replace(IMAGE, OTHER_IMAGE)).unwrap();

    let listed = run_toolbox(home.path(), StoreAction::List, None);
    assert!(listed.out.contains("NOT APPROVED"), "{}", listed.out);
}

#[test]
fn toolbox_revoke_leaves_the_manifest_installed() {
    let home = tempfile::tempdir().unwrap();
    install_toolbox(home.path(), "sandbox");
    run_toolbox(home.path(), StoreAction::Approve, Some("sandbox"));

    let revoked = run_toolbox(home.path(), StoreAction::Revoke, Some("sandbox"));
    assert_eq!(revoked.code, 0);
    assert!(revoked.out.contains("still installed"), "{}", revoked.out);
    assert!(
        home.path().join("toolboxes/sandbox/toolbox.json").exists(),
        "the files are not touched by a revocation"
    );
}

#[test]
fn approving_without_an_id_is_a_usage_error_rather_than_a_failure() {
    // Exit 2, which is what a shell script distinguishes from "it ran and said
    // no" — and the message names the command that lists the ids.
    let home = tempfile::tempdir().unwrap();
    let run = run_toolbox(home.path(), StoreAction::Approve, None);
    assert_eq!(run.code, 2);
    assert!(run.err.contains("ghostai toolbox list"), "{}", run.err);
}

#[test]
fn approving_something_that_is_not_installed_is_a_refusal_with_a_sentence() {
    let home = tempfile::tempdir().unwrap();
    let run = run_toolbox(home.path(), StoreAction::Approve, Some("nope"));
    assert_eq!(run.code, 1);
    assert!(!run.err.is_empty(), "a refusal says why");
}

// extension

#[test]
fn extension_list_says_where_it_looked_when_nothing_is_installed() {
    let home = tempfile::tempdir().unwrap();
    let run = run_extension(home.path(), StoreAction::List, None);
    assert_eq!(run.code, 0);
    assert!(
        run.out.contains("No extensions installed under"),
        "{}",
        run.out
    );
}

#[test]
fn extension_list_shows_the_command_and_what_it_contributes() {
    // The line that matters most, so it is never abbreviated away: an extension
    // declaring nothing can still run arbitrary code, and one declaring `tools`
    // is asking for something an operator has to grant per agent afterwards.
    let home = tempfile::tempdir().unwrap();
    install_extension(home.path(), "hello");
    let run = run_extension(home.path(), StoreAction::List, None);

    assert_eq!(run.code, 0);
    assert!(run.out.contains("hello"), "{}", run.out);
    assert!(run.out.contains("UNAPPROVED"), "{}", run.out);
    assert!(run.out.contains("node index.mjs"), "{}", run.out);
    assert!(run.out.contains("tools"), "{}", run.out);
    assert!(run.out.contains("1.2.3"), "{}", run.out);
}

#[test]
fn extension_approve_records_a_digest_over_every_byte() {
    // One step stronger than the toolbox's: a toolbox manifest pins an
    // immutable image, so hashing the manifest hashes the code; an extension
    // manifest names a command, so this hashes the whole directory.
    let home = tempfile::tempdir().unwrap();
    install_extension(home.path(), "hello");

    let approved = run_extension(home.path(), StoreAction::Approve, Some("hello"));
    assert_eq!(approved.code, 0);
    assert!(approved.out.contains("Approved hello"), "{}", approved.out);
    assert!(
        approved.out.contains("digest     sha256:"),
        "{}",
        approved.out
    );
    assert!(
        approved.out.contains("changes the digest"),
        "{}",
        approved.out
    );

    let listed = run_extension(home.path(), StoreAction::List, None);
    assert!(listed.out.contains("[approved]"), "{}", listed.out);
}

#[test]
fn editing_any_file_under_the_directory_moves_the_digest() {
    // Including one the manifest does not name: the digest is over every byte,
    // which is what makes an approval a statement about the install rather than
    // about its entry point.
    let home = tempfile::tempdir().unwrap();
    install_extension(home.path(), "hello");
    run_extension(home.path(), StoreAction::Approve, Some("hello"));

    std::fs::write(
        home.path().join("extensions/hello/index.mjs"),
        "// something else",
    )
    .unwrap();

    let listed = run_extension(home.path(), StoreAction::List, None);
    assert!(
        !listed.out.contains("[approved]"),
        "an edited install is no longer the one that was approved:\n{}",
        listed.out
    );
}

#[test]
fn extension_revoke_leaves_the_files_installed() {
    let home = tempfile::tempdir().unwrap();
    install_extension(home.path(), "hello");
    run_extension(home.path(), StoreAction::Approve, Some("hello"));

    let revoked = run_extension(home.path(), StoreAction::Revoke, Some("hello"));
    assert_eq!(revoked.code, 0);
    assert!(revoked.out.contains("still installed"), "{}", revoked.out);
    assert!(home.path().join("extensions/hello/index.mjs").exists());
}

#[test]
fn extension_approving_without_an_id_names_the_listing_command() {
    let home = tempfile::tempdir().unwrap();
    let run = run_extension(home.path(), StoreAction::Approve, None);
    assert_eq!(run.code, 2);
    assert!(run.err.contains("ghostai extension list"), "{}", run.err);
}

#[test]
fn an_empty_id_is_the_same_as_none() {
    // A shell that expanded an unset variable produces one, and treating it as
    // an id would look up an extension called "".
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        run_extension(home.path(), StoreAction::Approve, Some("")).code,
        2
    );
    assert_eq!(
        run_toolbox(home.path(), StoreAction::Approve, Some("")).code,
        2
    );
}
