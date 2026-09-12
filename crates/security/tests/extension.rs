//! Extension manifests, policy and the install digest, against
//! `fixtures/extension/digest`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::path::{Path, PathBuf};

use ghostai_core::ErrorKind;
use ghostai_security::{
    MAX_EXTENSION_FILES, assert_extension_policy, extension_digest, manifest_hash, parse_extension,
    read_extension_manifest,
};
use serde_json::{Value, json};

use common::{fixtures_dir, kind_of, message_of, read_fixture, symlink, temp_base, write};

/// An install directory holding a manifest and the entry it names.
fn install(base: &Path, id: &str, overrides: &Value, files: &[(&str, &str)]) -> PathBuf {
    let dir = base.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut manifest = json!({"schema": "ghostai.extension/1", "id": id});
    for (key, value) in overrides.as_object().unwrap() {
        manifest[key] = value.clone();
    }
    write(
        &dir.join("ghostai.extension.json"),
        serde_json::to_string(&manifest).unwrap(),
    );
    for (path, content) in files {
        write(&dir.join(path), content);
    }
    dir
}

const ENTRY: &[(&str, &str)] = &[("dist/index.js", "export const x = 1;\n")];

fn policy(dir: &Path) -> ghostai_core::Result<()> {
    assert_extension_policy(&read_extension_manifest(dir).unwrap(), dir)
}

#[test]
fn matches_the_digest_fixture() {
    let fixture = read_fixture("extension/digest/expected.json");
    let tree = fixtures_dir().join("extension/digest/tree");
    assert!(
        tree.join("link.txt")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fixture["symlinksWalked"], json!(false));

    assert_eq!(
        extension_digest(&tree).unwrap(),
        fixture["digest"].as_str().unwrap()
    );

    // The recorded lines reproduce the digest, so the intermediate the fixture
    // carries is the one this port computes.
    let lines: Vec<&str> = fixture["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap())
        .collect();
    assert_eq!(
        manifest_hash(lines.join("\n").as_bytes()),
        fixture["digest"].as_str().unwrap()
    );
    let mut by_bytes = lines.clone();
    by_bytes.sort_unstable();
    assert_ne!(
        by_bytes, lines,
        "the fixture tree must distinguish byte order from UTF-16 order"
    );
}

#[test]
fn parses_a_manifest_and_fills_the_defaults() {
    let manifest = parse_extension(br#"{"schema":"ghostai.extension/1","id":"slack"}"#).unwrap();
    assert_eq!(manifest.id, "slack");
    assert_eq!(manifest.entry, "dist/index.js");
}

#[test]
fn parse_errors_name_the_field() {
    assert!(message_of(&parse_extension(b"{")).contains("not valid JSON"));
    assert!(message_of(&parse_extension(br#"{"schema":"ghostai.extension/1"}"#)).contains("id"));
    assert!(message_of(&parse_extension(br#""a string""#)).contains("(root)"));
    assert!(
        message_of(&parse_extension(
            br#"{"schema":"ghostai.plugin/1","id":"slack"}"#
        ))
        .contains("not valid")
    );
}

#[test]
fn reads_the_manifest_an_install_directory_holds() {
    let (_dir, base) = temp_base();
    let dir = install(&base, "slack", &json!({"version": "2.0.0"}), ENTRY);
    assert_eq!(read_extension_manifest(&dir).unwrap().version, "2.0.0");
    let missing = read_extension_manifest(&base.join("nothing")).unwrap_err();
    assert_eq!(missing.kind, ErrorKind::Config);
}

#[test]
fn accepts_a_plain_install() {
    let (_dir, base) = temp_base();
    let dir = install(&base, "slack", &json!({}), ENTRY);
    assert!(policy(&dir).is_ok());
    let mjs = install(
        &base,
        "zulip",
        &json!({"entry": "dist/index.mjs"}),
        &[("dist/index.mjs", "")],
    );
    assert!(policy(&mjs).is_ok());
}

#[test]
fn refuses_a_bad_id_or_a_directory_that_disagrees() {
    let (_dir, base) = temp_base();
    let shouted = install(&base, "Slack", &json!({}), ENTRY);
    assert!(message_of(&policy(&shouted)).contains("not a usable extension id"));

    let disagrees = install(&base, "slack", &json!({"id": "notslack"}), ENTRY);
    let error = policy(&disagrees).unwrap_err();
    assert!(
        error
            .message
            .contains("installed in a directory called \"slack\"")
    );
    assert_eq!(error.details["dirName"], json!("slack"));
}

#[test]
fn refuses_entries_that_are_absolute_not_modules_or_outside() {
    let (_dir, base) = temp_base();
    let absolute = install(&base, "slack", &json!({"entry": "/etc/passwd.js"}), ENTRY);
    assert!(message_of(&policy(&absolute)).contains("absolute entry"));

    let cjs = install(
        &base,
        "slack",
        &json!({"entry": "dist/index.cjs"}),
        &[("dist/index.cjs", "module.exports = {};\n")],
    );
    assert!(message_of(&policy(&cjs)).contains("not an ES module"));

    write(&base.join("outside.js"), "export const x = 1;\n");
    let lexical = install(&base, "slack", &json!({"entry": "../outside.js"}), ENTRY);
    assert!(message_of(&policy(&lexical)).contains("outside its own directory"));

    let elsewhere = base.join("elsewhere");
    write(&elsewhere.join("index.js"), "export const x = 1;\n");
    let linked = install(&base, "corp", &json!({"entry": "lib/index.js"}), ENTRY);
    symlink(&elsewhere, &linked.join("lib"));
    let error = policy(&linked).unwrap_err();
    assert!(error.message.contains("outside its own directory"));
    assert_eq!(
        error.details["resolved"],
        json!(elsewhere.join("index.js").to_string_lossy())
    );

    let missing = install(&base, "slack", &json!({"entry": "dist/missing.js"}), ENTRY);
    assert_eq!(kind_of(&policy(&missing)), "not_found");
}

#[test]
fn the_digest_covers_the_code_not_only_the_manifest() {
    let (_dir, base) = temp_base();
    let dir = install(&base, "slack", &json!({}), ENTRY);
    let before = extension_digest(&dir).unwrap();

    write(&dir.join("dist").join("index.js"), "export const x = 2;\n");
    let edited = extension_digest(&dir).unwrap();
    assert_ne!(edited, before);

    write(&dir.join("dist").join("extra.js"), "export const y = 1;\n");
    let added = extension_digest(&dir).unwrap();
    assert_ne!(added, edited);

    std::fs::remove_file(dir.join("dist").join("extra.js")).unwrap();
    std::fs::remove_file(dir.join("dist").join("index.js")).unwrap();
    write(
        &dir.join("dist").join("renamed.js"),
        "export const x = 2;\n",
    );
    assert_ne!(extension_digest(&dir).unwrap(), edited);
}

#[test]
fn the_digest_is_stable_across_identical_installs_and_walks_nested_directories() {
    let (_dir, base) = temp_base();
    let a = install(&base, "slack", &json!({}), ENTRY);
    let b = install(&base, "slack-two", &json!({}), ENTRY);
    write(
        &b.join("ghostai.extension.json"),
        std::fs::read(a.join("ghostai.extension.json")).unwrap(),
    );
    assert_eq!(extension_digest(&b).unwrap(), extension_digest(&a).unwrap());

    let nested = install(
        &base,
        "deep",
        &json!({}),
        &[
            ("dist/index.js", "export const x = 1;\n"),
            ("dist/nested/deep/thing.js", "export const y = 1;\n"),
        ],
    );
    let before = extension_digest(&nested).unwrap();
    write(
        &nested.join("dist/nested/deep/thing.js"),
        "export const y = 2;\n",
    );
    assert_ne!(extension_digest(&nested).unwrap(), before);
}

#[test]
fn the_digest_does_not_follow_symlinks() {
    let (_dir, base) = temp_base();
    let dir = install(&base, "slack", &json!({}), ENTRY);
    let before = extension_digest(&dir).unwrap();
    write(&base.join("outside.js"), "stolen");
    symlink(&base.join("outside.js"), &dir.join("link.js"));
    symlink(&base, &dir.join("host"));
    assert_eq!(extension_digest(&dir).unwrap(), before);
}

#[test]
fn refuses_a_directory_with_more_files_than_an_extension_has() {
    let (_dir, base) = temp_base();
    let dir = install(&base, "slack", &json!({}), ENTRY);
    let many = dir.join("many");
    std::fs::create_dir_all(&many).unwrap();
    for index in 0..=MAX_EXTENSION_FILES {
        write(&many.join(format!("{index}.txt")), "x");
    }
    let error = extension_digest(&dir).unwrap_err();
    assert!(error.message.contains("too large to authorise"));
    assert_eq!(error.kind, ErrorKind::Config);
}

#[test]
fn the_digest_reports_a_directory_it_cannot_read() {
    let (_dir, base) = temp_base();
    assert_eq!(
        kind_of(&extension_digest(&base.join("nothing"))),
        "not_found"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = install(&base, "slack", &json!({}), ENTRY);
        let sealed = dir.join("sealed");
        write(&sealed.join("x.js"), "x");
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
        let outcome = extension_digest(&dir);
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(kind_of(&outcome), "config");
    }
}

/// A `ghostai.extension/2` install: a manifest naming an argv, and the files it
/// names.
fn install_v2(base: &Path, id: &str, command: &Value, files: &[(&str, &str)]) -> PathBuf {
    let dir = base.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir.join("ghostai.extension.json"),
        serde_json::to_string(&json!({
            "schema": "ghostai.extension/2",
            "id": id,
            "command": command,
        }))
        .unwrap(),
    );
    for (path, content) in files {
        write(&dir.join(path), content);
    }
    dir
}

const SCRIPT: &[(&str, &str)] = &[("index.mjs", "process.exit(0);\n")];

#[test]
fn a_v2_manifest_fills_the_argv_defaults() {
    let manifest =
        parse_extension(br#"{"schema":"ghostai.extension/2","id":"slack","command":["node"]}"#)
            .unwrap();
    assert_eq!(manifest.command, vec!["node".to_owned()]);
    // Names, never values: the four a child gets without asking.
    assert_eq!(manifest.env, vec!["PATH", "HOME", "LANG", "TMPDIR"]);
    assert!(manifest.providers.is_empty());
}

#[test]
fn accepts_a_bare_program_resolved_on_the_host_path() {
    let (_dir, base) = temp_base();
    let dir = install_v2(&base, "slack", &json!(["node", "index.mjs"]), SCRIPT);
    assert!(policy(&dir).is_ok());
}

#[test]
fn accepts_a_relative_program_inside_the_install() {
    let (_dir, base) = temp_base();
    let dir = install_v2(&base, "slack", &json!(["./bin/run", "--serve"]), &[]);
    write(&dir.join("bin/run"), "#!/bin/sh\n");
    assert!(policy(&dir).is_ok());
}

#[test]
fn refuses_a_v2_manifest_that_names_no_command() {
    let (_dir, base) = temp_base();
    let empty = install_v2(&base, "slack", &json!([]), SCRIPT);
    assert!(message_of(&policy(&empty)).contains("names no command"));
    assert_eq!(kind_of(&policy(&empty)), "config");

    let blank = install_v2(&base, "corp", &json!([""]), SCRIPT);
    assert!(message_of(&policy(&blank)).contains("names no command"));
}

#[test]
fn refuses_a_shell_as_the_program() {
    let (_dir, base) = temp_base();
    // Compared exactly as the exec guard compares it — `binary_name` strips a
    // directory and an executable suffix and nothing else.
    for program in ["bash", "sh", "/bin/zsh", "pwsh.exe"] {
        let dir = install_v2(&base, "slack", &json!([program, "-c", "echo hi"]), SCRIPT);
        assert!(
            message_of(&policy(&dir)).contains("names a shell"),
            "{program} was not refused as a shell"
        );
    }
}

#[test]
fn refuses_a_program_that_is_absolute_or_outside() {
    let (_dir, base) = temp_base();
    let absolute = install_v2(&base, "slack", &json!(["/usr/local/bin/node"]), SCRIPT);
    assert!(message_of(&policy(&absolute)).contains("absolute program"));

    write(&base.join("outside.mjs"), "process.exit(0);\n");
    let lexical = install_v2(&base, "slack", &json!(["../outside.mjs"]), SCRIPT);
    assert!(
        message_of(&policy(&lexical)).contains("outside its own directory"),
        "a relative escape was not refused"
    );

    let elsewhere = base.join("elsewhere");
    write(&elsewhere.join("run.mjs"), "process.exit(0);\n");
    let linked = install_v2(&base, "corp", &json!(["lib/run.mjs"]), SCRIPT);
    symlink(&elsewhere, &linked.join("lib"));
    let error = policy(&linked).unwrap_err();
    assert!(error.message.contains("outside its own directory"));
    assert_eq!(
        error.details["resolved"],
        json!(elsewhere.join("run.mjs").to_string_lossy())
    );

    let missing = install_v2(&base, "slack", &json!(["./nothing.mjs"]), SCRIPT);
    assert_eq!(kind_of(&policy(&missing)), "not_found");
}

#[test]
fn a_v1_manifest_is_still_held_to_the_entry_rule() {
    let (_dir, base) = temp_base();
    // The two branches do not leak into each other: a v1 manifest carrying a
    // `command` is still judged on its `entry`, and vice versa.
    let dir = base.join("slack");
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir.join("ghostai.extension.json"),
        r#"{"schema":"ghostai.extension/1","id":"slack","entry":"dist/index.cjs","command":["node","index.mjs"]}"#,
    );
    write(&dir.join("dist/index.cjs"), "module.exports = {};\n");
    write(&dir.join("index.mjs"), "process.exit(0);\n");
    assert!(message_of(&policy(&dir)).contains("not an ES module"));
}
