//! Finding the catalogue, and fetching it.
//!
//! The fetcher is injected throughout: nothing here reaches a registry, and the
//! one thing that matters about the real one — that the package lands at
//! `<prefix>/node_modules/@ghostwire/presets` — is asserted by building that
//! path with the same function the resolver reads it with, so the two cannot
//! drift apart.
//!
//! Every case names its own `near`, so the sibling-checkout lookup is pointed
//! at a temporary tree rather than at wherever the test binary happens to sit.
//! Without that, a reviewer who has the presets repository checked out beside
//! this one gets different results from CI, which is the flake the lookup is
//! most likely to cause.

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

use ghostai::catalogue::{
    CATALOGUE_ENV_VAR, CATALOGUE_PACKAGE, CATALOGUE_RANGE, CatalogueOptions, FetchCatalogueOptions,
    PRESETS_DIR_ENV_VAR, assert_catalogue_layout, catalogue_agents_dir, catalogue_container,
    catalogue_dir, catalogue_skill, catalogue_skills_dir, fetch_catalogue, fetched_catalogue_dir,
    sibling_search_roots,
};
use ghostai::i18n::Env;
use tempfile::TempDir;

/// A catalogue in the current layout: `agents/` and one build-context directory
/// per optional container.
fn write_catalogue(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir.join("agents")).unwrap();
    let context = dir.join("containers").join("dev");
    std::fs::create_dir_all(&context).unwrap();
    std::fs::write(context.join("Dockerfile"), "FROM scratch\n").unwrap();
    std::fs::write(context.join("container.yaml"), "{}").unwrap();
    dir.to_path_buf()
}

/// Options that cannot reach a real sibling checkout.
fn options(near: &Path) -> CatalogueOptions {
    CatalogueOptions {
        near: vec![near.to_path_buf()],
        ..CatalogueOptions::default()
    }
}

#[test]
fn takes_an_explicit_directory_over_everything_else() {
    let root = TempDir::new().unwrap();
    let explicit = write_catalogue(&root.path().join("checkout"));
    write_catalogue(&fetched_catalogue_dir(&root.path().join("fetched")));

    let resolved = catalogue_dir(&CatalogueOptions {
        from: Some(explicit.to_string_lossy().into_owned()),
        catalogue_dir: Some(root.path().join("fetched")),
        ..options(root.path())
    });

    assert_eq!(resolved, Some(explicit));
}

#[test]
fn reads_the_same_explicit_directory_from_the_environment() {
    let root = TempDir::new().unwrap();
    let explicit = write_catalogue(&root.path().join("checkout"));

    let resolved = catalogue_dir(&CatalogueOptions {
        env: Env::from_iter([(CATALOGUE_ENV_VAR, explicit.to_string_lossy().into_owned())]),
        ..options(root.path())
    });

    assert_eq!(resolved, Some(explicit));
}

#[test]
fn reads_a_fixed_install_from_its_own_variable() {
    // Separate from `GHOSTAI_CATALOGUE` because the two are set by different
    // people: one by somebody writing a preset, one by whoever laid the image
    // down.
    let root = TempDir::new().unwrap();
    let fixed = write_catalogue(&root.path().join("image-layer"));

    let resolved = catalogue_dir(&CatalogueOptions {
        env: Env::from_iter([(PRESETS_DIR_ENV_VAR, fixed.to_string_lossy().into_owned())]),
        ..options(root.path())
    });

    assert_eq!(resolved, Some(fixed));
}

#[test]
fn prefers_the_checkout_variable_over_the_fixed_one() {
    let root = TempDir::new().unwrap();
    let checkout = write_catalogue(&root.path().join("checkout"));
    let fixed = write_catalogue(&root.path().join("image-layer"));

    let resolved = catalogue_dir(&CatalogueOptions {
        env: Env::from_iter([
            (CATALOGUE_ENV_VAR, checkout.to_string_lossy().into_owned()),
            (PRESETS_DIR_ENV_VAR, fixed.to_string_lossy().into_owned()),
        ]),
        ..options(root.path())
    });

    assert_eq!(resolved, Some(checkout));
}

#[test]
fn does_not_fall_through_when_the_explicit_directory_is_missing() {
    // Pointing `--from` at a typo and silently getting the fetched copy is how
    // somebody ships a preset they never actually ran.
    let root = TempDir::new().unwrap();
    write_catalogue(&fetched_catalogue_dir(&root.path().join("fetched")));

    let resolved = catalogue_dir(&CatalogueOptions {
        from: Some(root.path().join("nope").to_string_lossy().into_owned()),
        catalogue_dir: Some(root.path().join("fetched")),
        ..options(root.path())
    });

    assert_eq!(resolved, None);
}

#[test]
fn finds_the_fetched_copy_under_the_prefix() {
    let root = TempDir::new().unwrap();
    let prefix = root.path().join("catalogue");
    let fetched = write_catalogue(&fetched_catalogue_dir(&prefix));

    let resolved = catalogue_dir(&CatalogueOptions {
        catalogue_dir: Some(prefix),
        ..options(root.path())
    });

    assert_eq!(resolved, Some(fetched));
}

#[test]
fn finds_a_sibling_checkout_last() {
    // What a contributor with both repositories checked out has. Last, so a
    // fetched copy an operator asked for still wins.
    let root = TempDir::new().unwrap();
    let sibling = write_catalogue(&root.path().join("GhostAI-presets"));

    let resolved = catalogue_dir(&options(root.path()));

    assert_eq!(resolved, Some(sibling));
}

#[test]
fn accepts_the_lower_cased_sibling_spelling_too() {
    // The spelling that comes back is the probe's, not the disk's: macOS and
    // Windows are case-insensitive, so both names resolve to the one directory
    // and the first probe wins. What is asserted is therefore that the
    // lower-cased clone is *found*, which is the property the two names exist
    // for.
    let root = TempDir::new().unwrap();
    write_catalogue(&root.path().join("ghostai-presets"));

    let resolved = catalogue_dir(&options(root.path())).expect("the sibling checkout");

    assert!(catalogue_agents_dir(&resolved).is_some(), "{resolved:?}");
    let name = resolved
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_lowercase();
    assert_eq!(name, "ghostai-presets");
}

#[test]
fn ignores_a_sibling_that_is_not_a_catalogue() {
    // The one lookup nobody named, so it has to recognise a catalogue rather
    // than a directory: a stale checkout with its presets under `presets/`
    // would otherwise produce a refusal about a directory the operator never
    // mentioned.
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("GhostAI-presets").join("presets")).unwrap();

    assert_eq!(catalogue_dir(&options(root.path())), None);
}

#[test]
fn names_the_agents_subdirectory_and_nothing_that_is_absent() {
    let root = TempDir::new().unwrap();
    let dir = write_catalogue(&root.path().join("c"));

    assert_eq!(catalogue_agents_dir(&dir), Some(dir.join("agents")));
    assert_eq!(catalogue_agents_dir(&root.path().join("empty")), None);
}

#[test]
fn refuses_a_catalogue_with_no_agents_naming_the_version_it_wants() {
    // The 1.x layout: a directory that exists, resolves, and offers nothing.
    let root = TempDir::new().unwrap();
    let old = root.path().join("old");
    std::fs::create_dir_all(old.join("presets")).unwrap();

    let error = assert_catalogue_layout(&old).unwrap_err();

    assert_eq!(error.kind, ghostai_core::ErrorKind::Config);
    assert!(error.message.contains(CATALOGUE_RANGE), "{}", error.message);
    assert!(error.message.contains("agents/"), "{}", error.message);
}

#[test]
fn answers_with_a_build_context_only_when_the_definition_is_there_too() {
    // A build context with no `container.yaml` is a half-checkout, and an image
    // build would be the wrong error to report it with.
    let root = TempDir::new().unwrap();
    let dir = write_catalogue(&root.path().join("c"));
    std::fs::create_dir_all(dir.join("containers").join("halfway")).unwrap();

    assert_eq!(
        catalogue_container(&dir, "dev"),
        Some(dir.join("containers").join("dev"))
    );
    assert_eq!(catalogue_container(&dir, "halfway"), None);
    assert_eq!(catalogue_container(&dir, "nowhere"), None);
}

#[test]
fn treats_skills_as_optional() {
    // A catalogue that ships only agent presets is an ordinary catalogue, so
    // this is `None` rather than a trip through `assert_catalogue_layout`.
    let root = TempDir::new().unwrap();
    let bare = write_catalogue(&root.path().join("bare"));
    assert_eq!(catalogue_skills_dir(&bare), None);
    assert!(assert_catalogue_layout(&bare).is_ok());

    let dir = write_catalogue(&root.path().join("c"));
    std::fs::create_dir_all(dir.join("skills")).unwrap();
    assert_eq!(catalogue_skills_dir(&dir), Some(dir.join("skills")));
}

#[test]
fn answers_with_a_sheet_only_when_the_skill_file_is_there_too() {
    // The same argument `catalogue_container` makes: a directory with no sheet
    // would be copied, reported as installed, and then silently skipped when
    // the sheets are read — with nothing anywhere saying why.
    let root = TempDir::new().unwrap();
    let dir = write_catalogue(&root.path().join("c"));
    let sheet = dir.join("skills").join("code-review");
    std::fs::create_dir_all(&sheet).unwrap();
    std::fs::write(sheet.join("SKILL.md"), "").unwrap();
    std::fs::create_dir_all(dir.join("skills").join("halfway")).unwrap();

    assert_eq!(catalogue_skill(&dir, "code-review"), Some(sheet));
    assert_eq!(catalogue_skill(&dir, "halfway"), None);
    assert_eq!(catalogue_skill(&dir, "nowhere"), None);
    assert_eq!(
        catalogue_skill(&root.path().join("bare"), "code-review"),
        None
    );
}

#[test]
fn asks_npm_for_the_pinned_range_into_the_prefix() {
    let root = TempDir::new().unwrap();
    let prefix = root.path().join("catalogue");
    let argv = std::cell::RefCell::new(Vec::<String>::new());

    let error = fetch_catalogue(&FetchCatalogueOptions {
        catalogue_dir: &prefix,
        range: None,
        fetch: Some(&|args: &[String]| {
            argv.borrow_mut().extend_from_slice(args);
            Ok(0)
        }),
    })
    // The install is faked, so nothing lands and the existence check fires.
    .unwrap_err();
    assert!(
        error.message.contains("does not exist"),
        "{}",
        error.message
    );

    let argv = argv.borrow();
    assert!(argv.contains(&"--prefix".to_owned()), "{argv:?}");
    assert!(
        argv.contains(&prefix.to_string_lossy().into_owned()),
        "{argv:?}"
    );
    assert!(
        argv.contains(&format!("{CATALOGUE_PACKAGE}@{CATALOGUE_RANGE}")),
        "{argv:?}"
    );
    // A prefix is a place to put one package, not a project: re-running is how
    // an update happens, and neither a manifest nor a lockfile belongs there.
    assert!(argv.contains(&"--no-save".to_owned()), "{argv:?}");
    assert!(argv.contains(&"--no-package-lock".to_owned()), "{argv:?}");
}

#[test]
fn answers_with_where_the_package_landed() {
    let root = TempDir::new().unwrap();
    let prefix = root.path().join("catalogue");

    let dir = fetch_catalogue(&FetchCatalogueOptions {
        catalogue_dir: &prefix,
        range: None,
        fetch: Some(&|_: &[String]| {
            write_catalogue(&fetched_catalogue_dir(&prefix));
            Ok(0)
        }),
    })
    .unwrap();

    assert_eq!(dir, fetched_catalogue_dir(&prefix));
}

#[test]
fn turns_a_non_zero_exit_into_the_sentence_with_the_way_out_in_it() {
    let root = TempDir::new().unwrap();

    let error = fetch_catalogue(&FetchCatalogueOptions {
        catalogue_dir: &root.path().join("c"),
        range: None,
        fetch: Some(&|_: &[String]| Ok(1)),
    })
    .unwrap_err();

    assert_eq!(error.kind, ghostai_core::ErrorKind::Tool);
    assert!(error.message.contains("--from"), "{}", error.message);
}

#[test]
fn passes_a_spawn_failure_through_rather_than_reporting_a_bad_exit() {
    // The two are different problems with different fixes: npm is not
    // installed, against npm ran and could not get the package.
    let root = TempDir::new().unwrap();

    let error = fetch_catalogue(&FetchCatalogueOptions {
        catalogue_dir: &root.path().join("c"),
        range: None,
        fetch: Some(&|_: &[String]| {
            Err(ghostai_core::GhostError::new(
                ghostai_core::ErrorKind::Tool,
                "Could not run npm: no such file",
            ))
        }),
    })
    .unwrap_err();

    assert!(
        error.message.contains("Could not run npm"),
        "{}",
        error.message
    );
}

#[test]
fn looks_beside_the_running_binary_when_nobody_named_a_place_to_look() {
    // The one lookup nobody configures. What is assertable without a checkout
    // beside this one is the shape: every root is an ancestor of the running
    // executable, nearest first, and the list is bounded so a binary installed
    // near the filesystem root does not walk to `/`.
    let roots = sibling_search_roots();

    assert!(!roots.is_empty(), "the test binary has a path");
    let exe = std::env::current_exe().unwrap();
    assert_eq!(roots[0], exe.parent().unwrap());
    for pair in roots.windows(2) {
        assert_eq!(pair[1], pair[0].parent().unwrap());
    }
    assert!(roots.len() <= 6, "{} roots is unbounded", roots.len());
}

#[test]
fn an_unnamed_lookup_falls_through_to_the_binary_s_own_ancestors() {
    // `near` empty is the production path, and it has to answer rather than
    // refuse: on a machine with no checkout beside this one the answer is
    // simply nothing, which is what the caller's own refusal is written for.
    let resolved = catalogue_dir(&CatalogueOptions::default());

    // Whether one is found depends on the machine, so what is pinned is that
    // the lookup only ever answers with a real catalogue.
    if let Some(found) = resolved {
        assert!(
            catalogue_agents_dir(&found).is_some(),
            "{}",
            found.display()
        );
    }
}

#[test]
fn the_fetch_options_keep_the_injected_fetcher_out_of_the_debug_output() {
    // A closure has no useful rendering; the prefix and the version range are
    // the two things worth reading out of a log line.
    let root = TempDir::new().unwrap();
    let rendered = format!(
        "{:?}",
        FetchCatalogueOptions {
            catalogue_dir: root.path(),
            range: Some("1.2.3"),
            fetch: Some(&|_: &[String]| Ok(0)),
        }
    );

    assert!(rendered.contains("FetchCatalogueOptions"), "{rendered}");
    assert!(rendered.contains("1.2.3"), "{rendered}");
}
