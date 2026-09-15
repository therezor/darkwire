//! Where things live on disk.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ghostai_core::paths::{
    GhostPaths, HOME_ENV_VAR, ResolveGhostPaths, ensure_dir, expand_home, extension_data_dir_for,
    extension_dir_for, resolve_path, shared_dir_for, workspace_dir_for,
};
use ghostai_core::{ErrorKind, GhostError};

const HOME: &str = "/home/ghost";

fn home() -> PathBuf {
    PathBuf::from(HOME)
}

fn options() -> ResolveGhostPaths {
    ResolveGhostPaths {
        root: None,
        workspace: None,
        env: Some(HashMap::new()),
        home: Some(home()),
    }
}

fn default_paths() -> GhostPaths {
    GhostPaths::resolve(options()).unwrap()
}

fn err<T: std::fmt::Debug>(result: Result<T, GhostError>) -> GhostError {
    match result {
        Ok(value) => panic!("expected an error, got {value:?}"),
        Err(error) => error,
    }
}

mod expand_home_tests {
    use super::*;

    #[test]
    fn expands_a_bare_tilde() {
        assert_eq!(expand_home("~", &home()), home());
    }

    #[test]
    fn expands_a_tilde_rooted_path() {
        assert_eq!(
            expand_home("~/.ghostai/workspace", &home()),
            home().join(".ghostai/workspace")
        );
    }

    #[test]
    fn leaves_tilde_user_alone_rather_than_guessing() {
        // Resolving another account's home needs a passwd lookup, and a
        // directory literally named `~alice` is the predictable wrong answer.
        assert_eq!(
            expand_home("~alice/docs", &home()),
            PathBuf::from("~alice/docs")
        );
    }

    #[test]
    fn leaves_ordinary_paths_alone() {
        assert_eq!(
            expand_home("/var/data", &home()),
            PathBuf::from("/var/data")
        );
        assert_eq!(
            expand_home("relative/dir", &home()),
            PathBuf::from("relative/dir")
        );
        assert_eq!(expand_home("", &home()), PathBuf::from(""));
    }

    #[test]
    fn does_not_expand_a_tilde_that_is_not_leading() {
        assert_eq!(expand_home("/opt/~/x", &home()), PathBuf::from("/opt/~/x"));
    }
}

mod resolve_path_tests {
    use super::*;

    #[test]
    fn keeps_an_absolute_path_absolute() {
        assert_eq!(
            resolve_path("/var/data", Path::new("/base"), &home()),
            PathBuf::from("/var/data")
        );
    }

    #[test]
    fn resolves_a_relative_path_against_the_base() {
        assert_eq!(
            resolve_path("workspace", Path::new("/base"), &home()),
            PathBuf::from("/base/workspace")
        );
    }

    #[test]
    fn normalises_traversal() {
        assert_eq!(
            resolve_path("../sibling", Path::new("/base/dir"), &home()),
            PathBuf::from("/base/sibling")
        );
        assert_eq!(
            resolve_path("./a/./b/../c", Path::new("/base"), &home()),
            PathBuf::from("/base/a/c")
        );
        // Climbing past the root stays at the root.
        assert_eq!(
            resolve_path("../../../x", Path::new("/base"), &home()),
            PathBuf::from("/x")
        );
    }

    #[test]
    fn expands_a_tilde_before_resolving() {
        assert_eq!(
            resolve_path("~/data", Path::new("/base"), &home()),
            home().join("data")
        );
    }
}

mod resolve_ghost_paths {
    use super::*;

    #[test]
    fn derives_everything_from_the_default_root() {
        let paths = default_paths();
        let root = home().join(".ghostai");
        assert_eq!(
            paths,
            GhostPaths {
                root: root.clone(),
                workspace: root.join("workspace"),
                shared_dir: root.join("shared"),
                policy_dir: root.join("policy"),
                presets_dir: root.join("presets"),
                catalogue_dir: root.join("catalogue"),
                runs_dir: root.join("runs"),
                config_file: root.join("config.yaml"),
                db_file: root.join("ghost.db"),
                logs_dir: root.join("logs"),
                extensions_dir: root.join("extensions"),
                extension_data_dir: root.join("extension-data"),
                vault_file: root.join("vault.json"),
                key_file: root.join("vault.key"),
            }
        );
    }

    #[test]
    fn keeps_presets_and_the_catalogue_out_of_what_the_file_tools_can_write() {
        // A preset names an agent's system prompt and its tool permissions, so
        // one writable through `write_file` would let prompt injection compose
        // the agent that runs next.
        let paths = default_paths();
        assert!(!paths.presets_dir.starts_with(&paths.workspace));
        assert!(!paths.catalogue_dir.starts_with(&paths.workspace));
    }

    #[test]
    fn honours_the_home_environment_variable() {
        let paths = GhostPaths::resolve(ResolveGhostPaths {
            env: Some(HashMap::from([(
                HOME_ENV_VAR.to_owned(),
                "/srv/ghost".to_owned(),
            )])),
            ..options()
        })
        .unwrap();
        assert_eq!(paths.root, PathBuf::from("/srv/ghost"));
        assert_eq!(paths.db_file, PathBuf::from("/srv/ghost/ghost.db"));
    }

    #[test]
    fn expands_a_tilde_in_the_environment_variable() {
        let paths = GhostPaths::resolve(ResolveGhostPaths {
            env: Some(HashMap::from([(
                HOME_ENV_VAR.to_owned(),
                "~/ghost-data".to_owned(),
            )])),
            ..options()
        })
        .unwrap();
        assert_eq!(paths.root, home().join("ghost-data"));
    }

    #[test]
    fn lets_an_explicit_root_win_over_the_environment() {
        let paths = GhostPaths::resolve(ResolveGhostPaths {
            env: Some(HashMap::from([(
                HOME_ENV_VAR.to_owned(),
                "/from/env".to_owned(),
            )])),
            root: Some("/explicit".to_owned()),
            ..options()
        })
        .unwrap();
        assert_eq!(paths.root, PathBuf::from("/explicit"));
    }

    #[test]
    fn resolves_a_relative_root_against_the_working_directory() {
        let paths = GhostPaths::resolve(ResolveGhostPaths {
            root: Some("relative-root".to_owned()),
            ..options()
        })
        .unwrap();
        assert_eq!(
            paths.root,
            std::env::current_dir().unwrap().join("relative-root")
        );
    }

    #[test]
    fn resolves_a_relative_workspace_against_the_root_not_the_cwd() {
        // A service restarted from a different directory must not end up with a
        // different workspace while the database still points at the old one.
        let paths = GhostPaths::resolve(ResolveGhostPaths {
            root: Some("/srv/ghost".to_owned()),
            workspace: Some("files".to_owned()),
            ..options()
        })
        .unwrap();
        assert_eq!(paths.workspace, PathBuf::from("/srv/ghost/files"));
    }

    #[test]
    fn accepts_an_absolute_workspace_outside_the_root() {
        let paths = GhostPaths::resolve(ResolveGhostPaths {
            root: Some("/srv/ghost".to_owned()),
            workspace: Some("/mnt/data".to_owned()),
            ..options()
        })
        .unwrap();
        assert_eq!(paths.workspace, PathBuf::from("/mnt/data"));
    }

    #[test]
    fn expands_a_tilde_in_the_workspace() {
        let paths = GhostPaths::resolve(ResolveGhostPaths {
            workspace: Some("~/projects".to_owned()),
            ..options()
        })
        .unwrap();
        assert_eq!(paths.workspace, home().join("projects"));
    }
}

mod ensure_dir_tests {
    use super::*;

    #[test]
    fn creates_nested_directories_and_returns_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a").join("b").join("c");
        assert_eq!(ensure_dir(&target).unwrap(), target.as_path());
        assert!(target.is_dir());
    }

    #[test]
    fn is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("once");
        ensure_dir(&target).unwrap();
        ensure_dir(&target).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn creates_the_directory_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("private");
        ensure_dir(&target).unwrap();
        // The vault keyfile and every session transcript live under directories
        // created this way; the default umask would leave them world-readable.
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o077,
            0
        );
    }

    #[test]
    fn fails_where_a_file_is_in_the_way() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(ensure_dir(&file.join("child")).is_err());
    }
}

mod workspace_dir_for_tests {
    use super::*;

    #[test]
    fn maps_the_default_to_the_workspace_root_itself() {
        let paths = default_paths();
        assert_eq!(
            workspace_dir_for(&paths, "default").unwrap(),
            paths.workspace
        );
    }

    #[test]
    fn nests_a_named_workspace_under_the_root() {
        let paths = default_paths();
        assert_eq!(
            workspace_dir_for(&paths, "client-acme").unwrap(),
            paths.workspace.join("client-acme")
        );
    }

    #[test]
    fn refuses_anything_that_is_not_an_id() {
        let paths = default_paths();
        for id in ["..", "a/b", "Work", ""] {
            let error = err(workspace_dir_for(&paths, id));
            assert_eq!(error.kind, ErrorKind::InvalidInput);
            assert!(error.message.contains("Not a workspace id"), "{id:?}");
            assert_eq!(error.details["id"], id);
        }
    }
}

mod shared_dir_for_tests {
    use super::*;

    #[test]
    fn keys_the_shared_layer_by_workspace_not_by_agent() {
        let paths = default_paths();
        assert_eq!(
            shared_dir_for(&paths, "default").unwrap(),
            paths.shared_dir.join("default")
        );
        assert_eq!(
            shared_dir_for(&paths, "client-acme").unwrap(),
            paths.shared_dir.join("client-acme")
        );
    }

    #[test]
    fn stays_outside_the_workspace_too() {
        let paths = default_paths();
        assert!(
            !shared_dir_for(&paths, "default")
                .unwrap()
                .starts_with(&paths.workspace)
        );
    }

    #[test]
    fn refuses_anything_that_is_not_an_id() {
        let paths = default_paths();
        for id in ["..", "a/b", "Work", ""] {
            assert!(
                err(shared_dir_for(&paths, id))
                    .message
                    .contains("Not a workspace id"),
                "{id:?}"
            );
        }
    }
}

mod extension_dirs {
    use super::*;

    #[test]
    fn gives_each_extension_an_install_directory_of_its_own() {
        let paths = default_paths();
        assert_eq!(
            extension_dir_for(&paths, "slack").unwrap(),
            paths.extensions_dir.join("slack")
        );
    }

    #[test]
    fn keeps_what_an_extension_writes_out_of_what_was_approved() {
        // The approval is a digest over every byte under the install directory,
        // so state written in there would revoke the extension's own approval
        // on the first write. Sibling directories, never nested.
        let paths = default_paths();
        let install = extension_dir_for(&paths, "slack").unwrap();
        let data = extension_data_dir_for(&paths, "slack").unwrap();
        assert_eq!(data, paths.extension_data_dir.join("slack"));
        assert!(!data.starts_with(&install));
        assert!(!install.starts_with(&data));
    }

    #[test]
    fn keeps_both_outside_the_workspace_the_tools_can_reach() {
        let paths = default_paths();
        assert!(
            !extension_dir_for(&paths, "slack")
                .unwrap()
                .starts_with(&paths.workspace)
        );
        assert!(
            !extension_data_dir_for(&paths, "slack")
                .unwrap()
                .starts_with(&paths.workspace)
        );
    }

    #[test]
    fn refuses_anything_that_is_not_an_id() {
        let paths = default_paths();
        for id in ["..", "a/b", "Slack", "", "~evil"] {
            assert!(
                err(extension_dir_for(&paths, id))
                    .message
                    .contains("Not an extension id"),
                "{id:?}"
            );
            assert!(
                err(extension_data_dir_for(&paths, id))
                    .message
                    .contains("Not an extension id"),
                "{id:?}"
            );
        }
    }
}
