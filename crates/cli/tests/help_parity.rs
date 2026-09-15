//! What `docs/cli.md` promises, `--help` has to offer.
//!
//! The documentation is the contract a person reads before they type anything,
//! and the two drift in one direction: a flag is renamed in the tree and the
//! page keeps describing the old one, because nothing fails. This is what
//! fails.
//!
//! The check is deliberately one-directional — **every command and flag the
//! page names must appear in the help** — rather than an equality. The tree is
//! allowed to carry more than the page documents, and it does: `--ready-file`
//! and `serve --json` are features for a supervisor rather than for a person at
//! a keyboard, and neither has a row on that page. An equality test would
//! either force undocumented plumbing onto the page or force the page to list
//! everything, and both are worse than the page staying a page.
//!
//! Parsing the page rather than restating it is the other half. A second list
//! of flags inside this file would be a third thing to keep in step, and the
//! one that nobody reads.

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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ghostai::i18n::Translations;
use ghostai::program::render_help;

/// The page, read from the repository rather than embedded.
fn docs() -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/cli.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()))
}

/// One command's help page, by the path `ghostai help <path>` would take.
fn help(path: &[&str]) -> String {
    let t = Translations::default();
    let owned: Vec<String> = path.iter().map(|part| (*part).to_owned()).collect();
    render_help(&t, &owned).unwrap_or_else(|| panic!("no such command: {path:?}"))
}

/// Every long flag the page names, as `--name`.
///
/// Read out of the flag tables, whose rows begin with a backticked flag. A
/// mention in prose is not a promise; a row in a table is.
fn documented_flags(page: &str) -> BTreeSet<String> {
    let mut flags = BTreeSet::new();
    for line in page.lines() {
        let line = line.trim();
        if !line.starts_with("| `") {
            continue;
        }
        for token in line.split(['`', ' ', ',', '|', '<', '>']) {
            if token.starts_with("--") && token.len() > 2 {
                flags.insert(token.to_owned());
            }
        }
    }
    flags
}

#[test]
fn every_command_the_page_lists_is_in_the_help() {
    let help = help(&[]);
    // The seven the page's synopsis block names, plus `chat`, which it calls
    // the default rather than listing.
    for command in [
        "chat",
        "init",
        "serve",
        "preset",
        "agent",
        "toolbox",
        "extension",
        "help",
    ] {
        assert!(
            help.contains(command),
            "`{command}` is documented in docs/cli.md and missing from `ghostai --help`:\n{help}"
        );
    }
}

#[test]
fn every_subcommand_the_page_lists_is_in_its_parents_help() {
    for (parent, children) in [
        ("toolbox", &["list"][..]),
        ("container", &["list"][..]),
        ("extension", &["list", "approve", "revoke"][..]),
        ("agent", &["install", "list"][..]),
        ("preset", &["list", "install", "update"][..]),
    ] {
        let page = help(&[parent]);
        for child in children {
            assert!(
                page.contains(child),
                "`ghostai {parent} {child}` is documented and missing from its help:\n{page}"
            );
        }
    }
}

#[test]
fn every_global_flag_the_page_documents_is_in_the_help() {
    let help = help(&[]);
    for flag in [
        "--home",
        "--log-level",
        "--verbose",
        "--no-color",
        "--version",
        "--help",
    ] {
        assert!(
            help.contains(flag),
            "{flag} is documented and missing:\n{help}"
        );
    }
    // The two short forms the page promises beside them.
    assert!(help.contains("-v,"), "-v is documented as the version flag");
    assert!(help.contains("-h,"), "-h is documented as the help flag");
}

#[test]
fn every_flag_the_page_tabulates_appears_on_some_help_page() {
    // One pass over the whole page, because a flag's row does not say which
    // command owns it — and a flag that has moved between commands is still
    // documented and still reachable, which is what the reader cares about.
    let pages: String = [
        help(&[]),
        help(&["chat"]),
        help(&["serve"]),
        help(&["preset"]),
        help(&["preset", "install"]),
        help(&["agent"]),
        help(&["agent", "install"]),
        help(&["toolbox"]),
        help(&["extension"]),
        help(&["init"]),
    ]
    .join("\n");

    let mut missing = Vec::new();
    for flag in documented_flags(&docs()) {
        if !pages.contains(&flag) {
            missing.push(flag);
        }
    }
    assert!(
        missing.is_empty(),
        "docs/cli.md tabulates flags no help page offers: {missing:?}"
    );
}

#[test]
fn chat_offers_every_flag_its_section_documents() {
    let page = help(&["chat"]);
    for flag in [
        "--session",
        "--agent",
        "--model",
        "--provider",
        "--workspace",
        "--workspace-id",
        "--new",
        "--json",
        "--no-reasoning",
        "--no-tools",
    ] {
        assert!(
            page.contains(flag),
            "{flag} missing from chat's help:\n{page}"
        );
    }
    // The short forms, which the page's table spells out beside each long one.
    for short in ["-s,", "-a,", "-m,", "-p,", "-w,", "-W,"] {
        assert!(
            page.contains(short),
            "{short} missing from chat's help:\n{page}"
        );
    }
}

#[test]
fn serve_offers_every_flag_its_section_documents() {
    let page = help(&["serve"]);
    for flag in [
        "--host",
        "--port",
        "--workspace",
        "--password",
        "--username",
        "--ui",
    ] {
        assert!(
            page.contains(flag),
            "{flag} missing from serve's help:\n{page}"
        );
    }
    for short in ["-H,", "-P,", "-w,"] {
        assert!(
            page.contains(short),
            "{short} missing from serve's help:\n{page}"
        );
    }
}

#[test]
fn the_page_does_not_yet_document_the_supervisor_flags() {
    // The other direction, stated so the gap is a recorded decision rather than
    // something a later reader has to work out. `--ready-file` and `serve
    // --json` are real features and are absent from `docs/cli.md`; when the
    // page gains them this test is what says so.
    let page = help(&["serve"]);
    assert!(page.contains("--ready-file"));
    assert!(page.contains("--json"));

    let documented = documented_flags(&docs());
    assert!(
        !documented.contains("--ready-file"),
        "docs/cli.md now documents --ready-file; delete this test and let the \
         parity check above cover it"
    );
}

#[test]
fn the_environment_variables_the_page_names_are_read_somewhere() {
    // Not a help check — they have no help row — but the same class of drift:
    // the page promises them and only the code can say whether they are read.
    let page = docs();
    for name in [
        "GHOSTAI_HOME",
        "GHOSTAI_PASSWORD",
        "GHOSTAI_USERNAME",
        "GHOSTAI_LANG",
        "GHOSTAI_LOG_LEVEL",
        "GHOSTAI_DEBUG",
    ] {
        assert!(page.contains(name), "docs/cli.md stopped naming {name}");
    }
}
