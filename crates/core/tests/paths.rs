//! Where things live on disk.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use darkwire_core::paths::{
    HOME_ENV_VAR, ResolveWirePaths, WORKSPACES_ENV_VAR, WirePaths, ensure_dir, expand_home,
    extension_data_dir_for, extension_dir_for, resolve_path, shared_dir_for, workspace_dir_for,
};
use darkwire_core::{ErrorKind, WireError};

const HOME: &str = "/home/ghost";

fn home() -> PathBuf {
    PathBuf::from(HOME)
}

fn options() -> ResolveWirePaths {
    ResolveWirePaths {
        root: None,
        workspaces: None,
        env: Some(HashMap::new()),
        home: Some(home()),
    }
}

fn default_paths() -> WirePaths {
    WirePaths::resolve(options()).unwrap()
}

fn err<T: std::fmt::Debug>(result: Result<T, WireError>) -> WireError {
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
            expand_home("~/.darkwire/workspace", &home()),
            home().join(".darkwire/workspace")
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

mod resolve_wire_paths {
    use super::*;

    #[test]
    fn derives_everything_from_the_default_root() {
        let paths = default_paths();
        let root = home().join(".darkwire");
        assert_eq!(
            paths,
            WirePaths {
                root: root.clone(),
                workspaces_dir: home().join("DarkWire/workspaces"),
                shared_dir: root.join("shared"),
                policy_dir: root.join("policy"),
                runs_dir: root.join("runs"),
                config_file: root.join("config.yaml"),
                db_file: root.join("darkwire.db"),
                logs_dir: root.join("logs"),
                extensions_dir: root.join("extensions"),
                extension_data_dir: root.join("extension-data"),
                vault_file: root.join("vault.json"),
                key_file: root.join("vault.key"),
            }
        );
    }

    #[test]
    fn keeps_the_policy_directory_out_of_what_the_file_tools_can_write() {
        // A definition names what a container may do, so one writable through
        // `write` would let prompt injection widen the box it runs in.
        let paths = default_paths();
        assert!(!paths.policy_dir.starts_with(&paths.workspaces_dir));
    }

    #[test]
    fn puts_the_workspaces_beside_the_root_rather_than_inside_it() {
        // The root holds the vault, the database and the policy tree, and each
        // workspace is a jail root. Nesting one in the other is what this
        // layout exists to avoid.
        let paths = default_paths();
        assert!(!paths.workspaces_dir.starts_with(&paths.root));
        assert!(!paths.root.starts_with(&paths.workspaces_dir));
    }

    #[test]
    fn honours_the_home_environment_variable() {
        let paths = WirePaths::resolve(ResolveWirePaths {
            env: Some(HashMap::from([(
                HOME_ENV_VAR.to_owned(),
                "/srv/ghost".to_owned(),
            )])),
            ..options()
        })
        .unwrap();
        assert_eq!(paths.root, PathBuf::from("/srv/ghost"));
        assert_eq!(paths.db_file, PathBuf::from("/srv/ghost/darkwire.db"));
    }

    #[test]
    fn expands_a_tilde_in_the_environment_variable() {
        let paths = WirePaths::resolve(ResolveWirePaths {
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
        let paths = WirePaths::resolve(ResolveWirePaths {
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
        let paths = WirePaths::resolve(ResolveWirePaths {
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
    fn resolves_a_relative_workspaces_folder_against_the_root_not_the_cwd() {
        // A service restarted from a different directory must not end up with a
        // different tree while the database still points at the old one.
        let paths = WirePaths::resolve(ResolveWirePaths {
            root: Some("/srv/ghost".to_owned()),
            workspaces: Some("files".to_owned()),
            ..options()
        })
        .unwrap();
        assert_eq!(paths.workspaces_dir, PathBuf::from("/srv/ghost/files"));
    }

    #[test]
    fn accepts_an_absolute_workspaces_folder_outside_the_root() {
        let paths = WirePaths::resolve(ResolveWirePaths {
            root: Some("/srv/ghost".to_owned()),
            workspaces: Some("/mnt/data".to_owned()),
            ..options()
        })
        .unwrap();
        assert_eq!(paths.workspaces_dir, PathBuf::from("/mnt/data"));
    }

    #[test]
    fn expands_a_tilde_in_the_workspaces_folder() {
        let paths = WirePaths::resolve(ResolveWirePaths {
            workspaces: Some("~/projects".to_owned()),
            ..options()
        })
        .unwrap();
        assert_eq!(paths.workspaces_dir, home().join("projects"));
    }

    #[test]
    fn honours_the_workspaces_environment_variable() {
        let paths = WirePaths::resolve(ResolveWirePaths {
            env: Some(HashMap::from([(
                WORKSPACES_ENV_VAR.to_owned(),
                "/mnt/wire".to_owned(),
            )])),
            ..options()
        })
        .unwrap();
        assert_eq!(paths.workspaces_dir, PathBuf::from("/mnt/wire"));
    }

    #[test]
    fn lets_an_explicit_workspaces_folder_beat_the_environment_variable() {
        let paths = WirePaths::resolve(ResolveWirePaths {
            workspaces: Some("/mnt/flag".to_owned()),
            env: Some(HashMap::from([(
                WORKSPACES_ENV_VAR.to_owned(),
                "/mnt/env".to_owned(),
            )])),
            ..options()
        })
        .unwrap();
        assert_eq!(paths.workspaces_dir, PathBuf::from("/mnt/flag"));
    }

    #[test]
    fn treats_an_empty_workspaces_variable_as_unset_rather_than_as_the_root() {
        // The trap `DARKWIRE_HOME=` has: an empty value resolving to the root
        // would put every workspace beside the vault.
        let paths = WirePaths::resolve(ResolveWirePaths {
            env: Some(HashMap::from([(
                WORKSPACES_ENV_VAR.to_owned(),
                String::new(),
            )])),
            ..options()
        })
        .unwrap();
        assert_eq!(paths.workspaces_dir, home().join("DarkWire/workspaces"));
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
    fn gives_the_default_a_folder_like_every_other_workspace() {
        let paths = default_paths();
        assert_eq!(
            workspace_dir_for(&paths, "default").unwrap(),
            paths.workspaces_dir.join("default")
        );
    }

    #[test]
    fn makes_every_workspace_a_sibling_of_the_default() {
        let paths = default_paths();
        assert_eq!(
            workspace_dir_for(&paths, "client-acme").unwrap(),
            paths.workspaces_dir.join("client-acme")
        );
        assert_eq!(
            workspace_dir_for(&paths, "client-acme").unwrap().parent(),
            workspace_dir_for(&paths, "default").unwrap().parent()
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
                .starts_with(&paths.workspaces_dir)
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
                .starts_with(&paths.workspaces_dir)
        );
        assert!(
            !extension_data_dir_for(&paths, "slack")
                .unwrap()
                .starts_with(&paths.workspaces_dir)
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
