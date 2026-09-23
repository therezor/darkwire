//! The workspace jail, against `fixtures/jail/paths.json`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::path::{MAIN_SEPARATOR_STR, Path, PathBuf};
use std::sync::Arc;

use darkwire_core::ErrorKind;
use darkwire_security::{
    JailCheck, JailOptions, JailRejection, JailResolver, PathShape, WorkspaceJail, escape_refusal,
    led_outside, path_shapes, single_jail,
};
use proptest::prelude::*;
use serde_json::{Value, json};

use common::{cases, read_fixture, slashes, symlink, temp_base, write};

struct Workspace {
    _dir: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    outside: PathBuf,
}

fn workspace() -> Workspace {
    let (dir, base) = temp_base();
    let root = base.join("workspace");
    let outside = base.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    write(&outside.join("secret.txt"), "stolen");
    Workspace {
        _dir: dir,
        base,
        root,
        outside,
    }
}

fn jail_at(root: &Path) -> WorkspaceJail {
    WorkspaceJail::new(JailOptions::new(root)).unwrap()
}

fn accept(jail: &WorkspaceJail, input: &str) -> darkwire_security::JailAccept {
    match jail.check(input) {
        JailCheck::Accept(accept) => accept,
        JailCheck::Reject { rejection, message } => {
            panic!("{input:?} was refused ({rejection}): {message}")
        }
    }
}

fn under_root(root: &Path, path: &Path) -> bool {
    path == root || path.starts_with(root)
}

#[test]
fn matches_the_paths_fixture() {
    let fixture = read_fixture("jail/paths.json");
    let layout = &fixture["layout"];
    let (dir, base) = temp_base();
    let root = base.join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    for directory in layout["directories"].as_array().unwrap() {
        std::fs::create_dir_all(root.join(directory.as_str().unwrap())).unwrap();
    }
    for file in layout["files"].as_array().unwrap() {
        write(&root.join(file.as_str().unwrap()), "x");
    }
    for file in layout["outside"].as_array().unwrap() {
        write(&base.join(file.as_str().unwrap()), "stolen");
    }
    for (name, target) in layout["symlinks"].as_object().unwrap() {
        symlink(Path::new(target.as_str().unwrap()), &root.join(name));
    }
    let jail = jail_at(&root);
    let base_text = base.to_string_lossy().into_owned();
    let scrub = |text: &str| -> String {
        text.replace(&root.to_string_lossy().into_owned(), "<root>")
            .replace(&base_text, "<base>")
            .replace(&base_text[1..], "<base>")
    };

    let mut failures = Vec::new();
    for case in cases(&fixture) {
        let template = case["input"]["path"].as_str().unwrap();
        let input = template.replace("<root>", &root.to_string_lossy());
        let check = match jail.check(&input) {
            JailCheck::Accept(accept) => json!({
                "ok": true,
                "relative": scrub(&accept.relative.replace(MAIN_SEPARATOR_STR, "/")),
                "canonical": scrub(&slashes(accept.path.strip_prefix(&root).unwrap_or(&accept.path))),
                "rewrites": accept.rewrites,
            }),
            JailCheck::Reject { rejection, message } => json!({
                "ok": false,
                "rejection": rejection,
                "message": scrub(&message),
            }),
        };
        let actual = json!({"check": check, "shapes": path_shapes(&input)});
        if actual != case["output"] {
            failures.push(format!(
                "{}\n  expected {}\n  actual   {}",
                case["name"], case["output"], actual
            ));
        }
    }
    drop(dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(cases(&fixture).len(), 49);
}

#[test]
fn creates_the_root_when_it_is_missing() {
    let ws = workspace();
    let created = ws.base.join("fresh").join("nested");
    assert_eq!(jail_at(&created).root(), created.as_path());
}

#[test]
fn canonicalises_a_symlinked_root() {
    let ws = workspace();
    let link = ws.base.join("link-to-workspace");
    symlink(&ws.root, &link);
    let mut options = JailOptions::new(&link);
    options.create = false;
    assert_eq!(
        WorkspaceJail::new(options).unwrap().root(),
        ws.root.as_path()
    );
}

#[test]
fn refuses_a_missing_root_when_create_is_off() {
    let ws = workspace();
    let mut options = JailOptions::new(ws.base.join("nope"));
    options.create = false;
    let error = WorkspaceJail::new(options).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.contains("Workspace root is unusable"));
}

#[test]
fn reports_an_unusable_root_as_a_config_error() {
    let ws = workspace();
    write(&ws.base.join("a-file"), "x");
    let error = WorkspaceJail::new(JailOptions::new(
        ws.base.join("a-file").join("under-a-file"),
    ))
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
}

#[test]
fn resolves_plain_paths_and_symlinks_that_stay_inside() {
    let ws = workspace();
    let jail = jail_at(&ws.root);
    assert_eq!(jail.resolve("notes.md").unwrap(), ws.root.join("notes.md"));
    assert_eq!(
        jail.resolve("a/b/c/d.txt").unwrap(),
        ws.root.join("a").join("b").join("c").join("d.txt")
    );
    write(&ws.root.join("here.txt"), "x");
    assert_eq!(jail.resolve("here.txt").unwrap(), ws.root.join("here.txt"));
    assert_eq!(jail.resolve(".").unwrap(), ws.root);

    std::fs::create_dir_all(ws.root.join("real")).unwrap();
    write(&ws.root.join("real").join("file.txt"), "x");
    symlink(&ws.root.join("real"), &ws.root.join("alias"));
    assert_eq!(
        jail.resolve("alias/file.txt").unwrap(),
        ws.root.join("real").join("file.txt")
    );
}

#[test]
fn clamps_root_markers_into_the_workspace() {
    let ws = workspace();
    let jail = jail_at(&ws.root);
    let cases: &[(&str, &[&str], &[PathShape])] = &[
        ("~", &[], &[PathShape::HomePrefix]),
        (
            "~/.ssh/id_ed25519",
            &[".ssh", "id_ed25519"],
            &[PathShape::HomePrefix],
        ),
        ("/etc/passwd", &["etc", "passwd"], &[PathShape::Absolute]),
        (
            "C:\\Windows\\System32",
            &["Windows", "System32"],
            &[PathShape::Drive],
        ),
        (
            "\\\\server\\share\\file",
            &["server", "share", "file"],
            &[PathShape::Unc],
        ),
        (
            "a/b/../../../etc/passwd",
            &["etc", "passwd"],
            &[PathShape::Traversal],
        ),
        ("///", &[], &[PathShape::Unc]),
    ];
    for (input, segments, rewrites) in cases {
        let verdict = accept(&jail, input);
        let mut expected = ws.root.clone();
        for segment in *segments {
            expected.push(segment);
        }
        assert_eq!(verdict.path, expected, "{input}");
        assert_eq!(
            verdict.relative,
            segments.join(MAIN_SEPARATOR_STR),
            "{input}"
        );
        assert_eq!(verdict.rewrites, *rewrites, "{input}");
    }
    assert!(accept(&jail, "a/b.txt").rewrites.is_empty());
    assert_eq!(
        jail.resolve("a/~/b").unwrap(),
        ws.root.join("a").join("~").join("b")
    );
    assert_eq!(
        jail.accept("/etc/passwd").unwrap().rewrites,
        [PathShape::Absolute]
    );
}

#[test]
fn does_not_nest_the_workspace_root_inside_itself() {
    let ws = workspace();
    let jail = jail_at(&ws.root);
    let verdict = accept(&jail, &ws.root.join("notes").join("x.md").to_string_lossy());
    assert_eq!(verdict.path, ws.root.join("notes").join("x.md"));
    assert_eq!(verdict.relative, format!("notes{MAIN_SEPARATOR_STR}x.md"));
    assert_eq!(verdict.rewrites, [PathShape::Absolute]);

    let bare = accept(&jail, &ws.root.to_string_lossy());
    assert_eq!(bare.path, ws.root);
    assert_eq!(bare.relative, "");

    let sibling = accept(&jail, &format!("{}-other/notes/x.md", ws.root.display()));
    assert_ne!(sibling.relative, format!("notes{MAIN_SEPARATOR_STR}x.md"));
    assert!(sibling.relative.contains("notes"));

    // The relative form is a fixed point: it resolves to the same file again.
    let first = accept(&jail, &ws.root.join("a").join("b.txt").to_string_lossy());
    assert_eq!(jail.resolve(&first.relative).unwrap(), first.path);

    // A backslash-spelled root is the same root.
    let backslashed = accept(&jail, &ws.root.to_string_lossy().replace('/', "\\"));
    assert_eq!(backslashed.path, ws.root);
}

#[test]
fn re_folds_after_stripping_a_root_marker() {
    let ws = workspace();
    let verdict = accept(&jail_at(&ws.root), "./c:..");
    assert_eq!(verdict.path, ws.root);
    assert_eq!(verdict.relative, "");
}

#[test]
fn refuses_what_the_filesystem_cannot_answer_for() {
    let ws = workspace();
    let jail = jail_at(&ws.root);
    let refusals = [
        ("", JailRejection::Empty),
        ("notes\0.md", JailRejection::NulByte),
        ("ok.txt\0.png", JailRejection::NulByte),
    ];
    for (input, rejection) in refusals {
        assert_eq!(jail.check(input).rejection(), Some(rejection), "{input:?}");
    }
    assert_eq!(
        jail.check(&"x".repeat(4096)).rejection(),
        Some(JailRejection::Unverifiable)
    );
    assert!(!jail.check("").is_ok());
    assert!(jail.check("").accepted().is_none());
    assert!(jail.check("ok").rejection().is_none());
}

#[test]
fn refuses_symlinks_that_lead_out() {
    let ws = workspace();
    let jail = jail_at(&ws.root);
    symlink(&ws.outside, &ws.root.join("escape"));
    assert_eq!(
        jail.check("escape/secret.txt").rejection(),
        Some(JailRejection::OutsideRoot)
    );
    symlink(&ws.base, &ws.root.join("up"));
    assert!(!jail.check("up/outside/secret.txt").is_ok());
    symlink(&ws.outside.join("secret.txt"), &ws.root.join("alias.txt"));
    assert!(!jail.check("alias.txt").is_ok());
}

#[test]
fn the_root_refuses_a_directory_swapped_for_a_symlink_after_the_check() {
    let ws = workspace();
    let jail = jail_at(&ws.root);
    write(&ws.root.join("sub/secret.txt"), "mine");
    let accepted = accept(&jail, "sub/secret.txt");
    assert_eq!(jail.beneath(&accepted), PathBuf::from("sub/secret.txt"));

    std::fs::remove_dir_all(ws.root.join("sub")).unwrap();
    symlink(&ws.outside, &ws.root.join("sub"));
    let root = jail.open_root().unwrap();
    let error = root.open(jail.beneath(&accepted)).unwrap_err();
    assert!(led_outside(&error), "{error:?}");

    let refusal = escape_refusal("sub/secret.txt");
    assert_eq!(refusal.kind, ErrorKind::JailEscape);
    assert_eq!(
        refusal.message,
        "Path resolves outside the workspace: sub/secret.txt"
    );
    assert_eq!(refusal.details["rejection"], json!("outside_root"));
}

#[test]
fn the_root_follows_a_symlink_that_stays_inside() {
    let ws = workspace();
    let jail = jail_at(&ws.root);
    write(&ws.root.join("real/notes.md"), "inside");
    write(&ws.root.join("sub/notes.md"), "before");
    let accepted = accept(&jail, "sub/notes.md");

    std::fs::remove_dir_all(ws.root.join("sub")).unwrap();
    // Relative: the capability layer reads an absolute target as leaving.
    symlink(Path::new("real"), &ws.root.join("sub"));
    let root = jail.open_root().unwrap();
    let text = root.read_to_string(jail.beneath(&accepted)).unwrap();
    assert_eq!(text, "inside");
    assert_eq!(jail.beneath(&accept(&jail, ".")), PathBuf::from("."));
}

#[test]
fn a_real_permission_error_is_not_an_escape() {
    // `EACCES`, which is 13 on every Unix this builds for.
    let error = std::io::Error::from_raw_os_error(13);
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(!led_outside(&error));
    assert!(!led_outside(&std::io::Error::from(
        std::io::ErrorKind::NotFound
    )));
}

#[test]
fn refuses_a_dangling_symlink_rather_than_a_path_that_does_not_exist_yet() {
    let ws = workspace();
    let jail = jail_at(&ws.root);
    symlink(&ws.base.join("vault.json"), &ws.root.join("vault"));
    assert_eq!(
        jail.check("vault").rejection(),
        Some(JailRejection::Unverifiable)
    );
    symlink(&ws.base.join("gone"), &ws.root.join("link"));
    assert!(!jail.check("link/child.txt").is_ok());
}

#[test]
fn reports_a_deleted_root_as_unverifiable() {
    let ws = workspace();
    let jail = jail_at(&ws.root);
    std::fs::remove_dir_all(&ws.root).unwrap();
    assert_eq!(
        jail.check("notes.md").rejection(),
        Some(JailRejection::Unverifiable)
    );
}

#[test]
fn errors_carry_the_kind_and_the_rejection() {
    let ws = workspace();
    let jail = jail_at(&ws.root);
    symlink(&ws.outside, &ws.root.join("escape"));
    let error = jail.resolve("escape/secret.txt").unwrap_err();
    assert_eq!(error.kind, ErrorKind::JailEscape);
    assert!(!error.retryable);
    assert_eq!(error.details["rejection"], json!("outside_root"));

    let empty = jail.resolve("").unwrap_err();
    assert_eq!(empty.kind, ErrorKind::InvalidInput);
}

#[test]
fn path_shapes_classify_without_resolving() {
    assert_eq!(path_shapes("/etc/passwd"), [PathShape::Absolute]);
    assert_eq!(path_shapes("~/.ssh/id_ed25519"), [PathShape::HomePrefix]);
    assert_eq!(path_shapes("//server/share"), [PathShape::Unc]);
    assert_eq!(path_shapes("C:\\Windows"), [PathShape::Drive]);
    assert_eq!(path_shapes("../x"), [PathShape::Traversal]);
    assert_eq!(path_shapes("a/../b"), [PathShape::Traversal]);
    for input in [
        "src/index.ts",
        "./script.js",
        "--format=json",
        "https://example.com/a/b",
    ] {
        assert!(path_shapes(input).is_empty(), "{input}");
    }
}

#[test]
fn path_shapes_agree_with_what_check_reports() {
    let ws = workspace();
    let jail = jail_at(&ws.root);
    for input in ["/etc/passwd", "~/x", "../x", "a/../b", "src/a.ts", "C:\\x"] {
        assert_eq!(accept(&jail, input).rewrites, path_shapes(input), "{input}");
    }
}

#[test]
fn shapes_and_rejections_spell_themselves() {
    assert_eq!(PathShape::HomePrefix.to_string(), "home_prefix");
    assert_eq!(JailRejection::OutsideRoot.to_string(), "outside_root");
    assert_eq!(serde_json::to_value(PathShape::Unc).unwrap(), json!("unc"));
    assert_eq!(
        serde_json::to_value(JailRejection::NulByte).unwrap(),
        json!("nul_byte")
    );
    let shape: PathShape = serde_json::from_value(json!("drive")).unwrap();
    assert_eq!(shape, PathShape::Drive);
}

#[test]
fn single_jail_answers_the_same_jail_for_every_workspace() {
    let ws = workspace();
    let jail = Arc::new(jail_at(&ws.root));
    let resolver = single_jail(Arc::clone(&jail));
    assert_eq!(resolver.default_jail().root(), jail.root());
    assert_eq!(resolver.for_workspace("anything").root(), jail.root());
    let debug = format!("{resolver:?}");
    assert!(debug.contains("SingleJail"));
}

#[test]
fn contains_is_component_wise() {
    let ws = workspace();
    let jail = jail_at(&ws.root);
    assert!(jail.contains(&ws.root));
    assert!(jail.contains(&ws.root.join("a").join("b")));
    assert!(jail.contains(&ws.root.join("a").join("..").join("b")));
    assert!(!jail.contains(&PathBuf::from(format!("{}-evil", ws.root.display()))));
    assert!(!jail.contains(&ws.base));
    assert!(!jail.contains(&ws.outside.join("secret.txt")));
    assert!(!jail.contains(&ws.root.join("..")));
    // A relative path is resolved against the working directory, which is not
    // the workspace.
    assert!(!jail.contains(Path::new("relative/thing")));
}

#[test]
fn a_jail_at_the_filesystem_root_contains_everything_absolute() {
    let mut options = JailOptions::new("/");
    options.create = false;
    let at_root = WorkspaceJail::new(options).unwrap();
    assert!(at_root.contains(Path::new("/etc")));
    assert!(at_root.contains(Path::new("/")));
    // There is no prefix to strip, so an absolute path is clamped as written.
    let verdict = accept(&at_root, "/etc");
    assert_eq!(verdict.rewrites, [PathShape::Absolute]);
}

#[test]
fn folds_case_only_when_asked() {
    let ws = workspace();
    let mut sensitive = JailOptions::new(&ws.root);
    sensitive.case_insensitive = Some(false);
    let mut insensitive = JailOptions::new(&ws.root);
    insensitive.case_insensitive = Some(true);
    let sensitive = WorkspaceJail::new(sensitive).unwrap();
    let insensitive = WorkspaceJail::new(insensitive).unwrap();
    let shouted = PathBuf::from(ws.root.to_string_lossy().to_uppercase()).join("file.txt");
    assert!(!sensitive.contains(&shouted));
    assert!(insensitive.contains(&shouted));
    // The root prefix is recognised case-insensitively too.
    let verdict = accept(&insensitive, &shouted.to_string_lossy());
    assert_eq!(verdict.relative, "file.txt");
    let _ = insensitive.relative(&shouted).unwrap();
}

#[test]
fn relative_returns_the_workspace_relative_form() {
    let ws = workspace();
    let jail = jail_at(&ws.root);
    assert_eq!(
        jail.relative(&ws.root.join("a").join("b.txt")).unwrap(),
        format!("a{MAIN_SEPARATOR_STR}b.txt")
    );
    assert_eq!(jail.relative(&ws.root).unwrap(), "");
    let error = jail.relative(&ws.outside.join("secret.txt")).unwrap_err();
    assert_eq!(error.kind, ErrorKind::JailEscape);
    assert_eq!(error.details["rejection"], json!("outside_root"));
}

#[test]
fn a_jail_can_be_compared_and_printed() {
    let ws = workspace();
    let jail = jail_at(&ws.root);
    assert_eq!(jail, jail.clone());
    assert!(format!("{jail:?}").contains("WorkspaceJail"));
    assert!(format!("{:?}", JailOptions::new("x")).contains("JailOptions"));
    let verdict: Value = json!(accept(&jail, "x").rewrites);
    assert_eq!(verdict, json!([]));
}

fn escape_fragments() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "..".to_owned(),
        "../".to_owned(),
        "..\\".to_owned(),
        "~".to_owned(),
        "~/".to_owned(),
        "/".to_owned(),
        "//".to_owned(),
        "\\\\".to_owned(),
        "C:".to_owned(),
        "\0".to_owned(),
        ".".to_owned(),
        "a".to_owned(),
        "%2e%2e".to_owned(),
        "ﬁle".to_owned(),
        "e\u{0301}".to_owned(),
        "\u{00e9}".to_owned(),
        " ".to_owned(),
        "x".repeat(300),
    ])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn nothing_escapes_for_adversarial_fragments(fragments in prop::collection::vec(escape_fragments(), 1..=8)) {
        let ws = workspace();
        let jail = jail_at(&ws.root);
        if let JailCheck::Accept(verdict) = jail.check(&fragments.concat()) {
            prop_assert!(under_root(&ws.root, &verdict.path), "{}", verdict.path.display());
        }
    }

    #[test]
    fn clamping_is_idempotent(fragments in prop::collection::vec(escape_fragments(), 1..=8)) {
        // The REST layer echoes `relative` back to clients, which send it again.
        // A normalisation that moved on the second pass would walk somewhere new
        // on every round trip.
        let ws = workspace();
        let jail = jail_at(&ws.root);
        if let JailCheck::Accept(first) = jail.check(&fragments.concat()) {
            let again = if first.relative.is_empty() { "." } else { first.relative.as_str() };
            match jail.check(again) {
                JailCheck::Accept(second) => prop_assert_eq!(second.path, first.path),
                JailCheck::Reject { message, .. } => prop_assert!(false, "{message}"),
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn nothing_escapes_for_arbitrary_strings(input in any::<String>()) {
        let ws = workspace();
        let jail = jail_at(&ws.root);
        if let JailCheck::Accept(verdict) = jail.check(&input) {
            prop_assert!(under_root(&ws.root, &verdict.path));
        }
    }
}

proptest! {
    #[test]
    fn every_traversal_is_clamped_not_refused(segments in prop::collection::vec(prop::sample::select(vec!["a", "b", ".."]), 1..=6)) {
        prop_assume!(segments.contains(&".."));
        let ws = workspace();
        let jail = jail_at(&ws.root);
        match jail.check(&segments.join("/")) {
            JailCheck::Accept(verdict) => {
                prop_assert!(under_root(&ws.root, &verdict.path));
                prop_assert!(verdict.rewrites.contains(&PathShape::Traversal));
            }
            JailCheck::Reject { message, .. } => prop_assert!(false, "{message}"),
        }
    }

    #[test]
    fn segments_that_only_look_like_traversal_survive(segments in prop::collection::vec(prop::sample::select(vec!["..foo", ".hidden", "a..b", "x.", "plain"]), 1..=5)) {
        let ws = workspace();
        let jail = jail_at(&ws.root);
        match jail.check(&segments.join("/")) {
            JailCheck::Accept(verdict) => prop_assert_eq!(verdict.relative, segments.join(MAIN_SEPARATOR_STR)),
            JailCheck::Reject { message, .. } => prop_assert!(false, "{message}"),
        }
    }
}
