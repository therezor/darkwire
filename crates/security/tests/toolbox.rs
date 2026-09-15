//! Toolbox bundles: grant resolution, operation shape, and the egress request.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use ghostai_core::ErrorKind;
use ghostai_protocol::{ContainerNetwork, NetworkMode, ToolPermission};
use ghostai_security::{
    JailOptions, WorkspaceJail, assert_container_network, assert_slug, command_argv,
    narrow_permission, parse_toolbox, resolve_bundle, validate_input, validate_operation,
};
use serde_json::{Value, json};

use common::message_of;

fn operation() -> Value {
    json!({
        "schema": "ghostai.tool/1",
        "description": "Repository status",
        "implementation": {
            "kind": "command",
            "executable": "/usr/bin/git",
            "argv": ["status", "--porcelain"],
        },
        "parameters": {"type": "object", "properties": {}, "additionalProperties": false},
    })
}

fn manifest() -> Value {
    json!({
        "schema": "ghostai.toolbox/1",
        "name": "research",
        "tools": [{"name": "status", "definition": "status", "permission": "ask"}],
    })
}

/// A policy root holding one toolbox and the definition it names.
fn policy_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("toolboxes")).unwrap();
    std::fs::create_dir_all(root.path().join("tool-definitions")).unwrap();
    std::fs::write(
        root.path().join("toolboxes/research.yaml"),
        manifest().to_string(),
    )
    .unwrap();
    std::fs::write(
        root.path().join("tool-definitions/status.yaml"),
        operation().to_string(),
    )
    .unwrap();
    root
}

fn resolve(
    root: &tempfile::TempDir,
    manifest: &Value,
) -> ghostai_core::Result<ghostai_security::ResolvedToolbox> {
    resolve_bundle(root.path(), manifest.to_string().as_bytes())
}

fn parse_operation(value: &Value) -> ghostai_protocol::toolbox::ToolOperation {
    serde_json::from_value(value.clone()).unwrap()
}

#[test]
fn a_toolbox_carries_grants_and_nothing_that_could_widen_one() {
    let toolbox = parse_toolbox(manifest().to_string().as_bytes()).unwrap();
    assert_eq!(toolbox.name, "research");
    assert_eq!(toolbox.version, "0.0.0");
    assert_eq!(toolbox.tools[0].permission, ToolPermission::Ask);

    // Every field that decides where or how a command runs belongs to a
    // container, so a toolbox naming one is refused rather than ignored.
    for field in [
        "image",
        "network",
        "caps",
        "security",
        "limits",
        "container",
    ] {
        let mut smuggled = manifest();
        smuggled[field] = json!("anything");
        let message = message_of(&parse_toolbox(smuggled.to_string().as_bytes()));
        assert!(message.contains("not valid"), "{field}: {message}");
    }
}

#[test]
fn a_bundle_hashes_the_toolbox_and_every_definition_it_names() {
    let root = policy_root();
    let first = resolve(&root, &manifest()).unwrap();
    assert_eq!(first.operations.len(), 1);
    assert_eq!(first.digest.len(), 64);

    // Editing a shared definition changes the bundle hash, which is what
    // revokes every toolbox that reaches it.
    let mut changed = operation();
    changed["implementation"]["argv"] = json!(["push"]);
    std::fs::write(
        root.path().join("tool-definitions/status.yaml"),
        changed.to_string(),
    )
    .unwrap();
    let second = resolve(&root, &manifest()).unwrap();
    assert_ne!(first.digest, second.digest);
}

#[test]
fn a_definition_reference_cannot_leave_the_policy_directory() {
    let root = policy_root();
    for reference in [
        "../outside",
        "sub/dir",
        ".",
        "",
        "-leading",
        &"x".repeat(65),
    ] {
        let mut escaping = manifest();
        escaping["tools"][0]["definition"] = json!(reference);
        assert!(resolve(&root, &escaping).is_err(), "{reference}");
    }
    assert!(assert_slug("git-status").is_ok());
    assert!(assert_slug("A").is_err());
}

#[test]
fn a_bundle_refuses_a_missing_definition_and_a_duplicate_grant() {
    let root = policy_root();
    let mut absent = manifest();
    absent["tools"][0]["definition"] = json!("nowhere");
    assert!(message_of(&resolve(&root, &absent)).contains("Cannot read"));

    let mut duplicate = manifest();
    duplicate["tools"] = json!([
        {"name": "status", "definition": "status"},
        {"name": "status", "definition": "status"},
    ]);
    assert!(message_of(&resolve(&root, &duplicate)).contains("Duplicate"));

    let mut mismatched = manifest();
    mismatched["name"] = json!("Research");
    assert!(resolve(&root, &mismatched).is_err());
}

#[test]
fn operation_schemas_must_be_self_contained() {
    let root = policy_root();
    for reference in ["$ref", "$dynamicRef", "$recursiveRef"] {
        let mut remote = operation();
        remote["parameters"]["properties"][reference] = json!("https://example.com/s");
        remote["parameters"][reference] = json!("https://example.com/s");
        std::fs::write(
            root.path().join("tool-definitions/status.yaml"),
            remote.to_string(),
        )
        .unwrap();
        let message = message_of(&resolve(&root, &manifest()));
        assert!(message.contains("self-contained"), "{reference}: {message}");
    }
}

#[test]
fn operation_parameters_must_be_a_closed_object() {
    let mut open = operation();
    open["parameters"] = json!({"type": "object", "properties": {}});
    assert!(
        message_of(&validate_operation(&parse_operation(&open)))
            .contains("additionalProperties:false")
    );

    let mut not_object = operation();
    not_object["parameters"] = json!({"type": "string", "additionalProperties": false});
    assert!(validate_operation(&parse_operation(&not_object)).is_err());
}

#[test]
fn operation_executables_are_absolute_and_outside_the_workspace() {
    for executable in ["git", "/usr/../bin/git", "/usr/bin/g\0it"] {
        let mut relative = operation();
        relative["implementation"]["executable"] = json!(executable);
        let parsed = serde_json::from_value(relative);
        // A relative path is refused by the schema; the rest by the policy.
        if let Ok(parsed) = parsed {
            assert!(validate_operation(&parsed).is_err(), "{executable}");
        }
    }
    for executable in ["/workspace", "/workspace/tool"] {
        let mut inside = operation();
        inside["implementation"]["executable"] = json!(executable);
        assert!(
            message_of(&validate_operation(&parse_operation(&inside)))
                .contains("writable workspace"),
            "{executable}"
        );
    }
}

#[test]
fn argv_inputs_must_be_required_scalars() {
    let mut optional = operation();
    optional["parameters"]["properties"]["path"] = json!({"type": "string"});
    optional["implementation"]["argv"] = json!([{"input": "path"}]);
    assert!(
        message_of(&validate_operation(&parse_operation(&optional))).contains("required scalar")
    );

    let mut array = operation();
    array["parameters"]["properties"]["path"] = json!({"type": "array"});
    array["parameters"]["required"] = json!(["path"]);
    array["implementation"]["argv"] = json!([{"input": "path"}]);
    assert!(validate_operation(&parse_operation(&array)).is_err());

    let mut unknown = operation();
    unknown["implementation"]["argv"] = json!([{"input": "nowhere"}]);
    assert!(message_of(&validate_operation(&parse_operation(&unknown))).contains("Unknown argv"));

    let mut nul = operation();
    nul["implementation"]["argv"] = json!(["sta\0tus"]);
    assert!(message_of(&validate_operation(&parse_operation(&nul))).contains("NUL"));

    let mut path_number = operation();
    path_number["parameters"]["properties"]["path"] = json!({"type": "integer"});
    path_number["parameters"]["required"] = json!(["path"]);
    path_number["implementation"]["argv"] = json!([{"input": "path", "workspacePath": true}]);
    assert!(
        message_of(&validate_operation(&parse_operation(&path_number))).contains("Workspace paths")
    );

    let mut ok = operation();
    ok["parameters"]["properties"]["path"] = json!({"type": "string"});
    ok["parameters"]["required"] = json!(["path"]);
    ok["implementation"]["argv"] = json!([{"input": "path", "workspacePath": true}]);
    assert!(validate_operation(&parse_operation(&ok)).is_ok());
}

#[test]
fn argv_input_must_be_a_required_array_of_strings() {
    let mut wrong = operation();
    wrong["parameters"]["properties"]["args"] = json!({"type": "string"});
    wrong["parameters"]["required"] = json!(["args"]);
    wrong["implementation"]["argvInput"] = json!("args");
    assert!(message_of(&validate_operation(&parse_operation(&wrong))).contains("argvInput"));

    let mut unknown = operation();
    unknown["implementation"]["argvInput"] = json!("nowhere");
    assert!(validate_operation(&parse_operation(&unknown)).is_err());

    let mut ok = operation();
    ok["parameters"]["properties"]["args"] = json!({"type": "array", "items": {"type": "string"}});
    ok["parameters"]["required"] = json!(["args"]);
    ok["implementation"]["argvInput"] = json!("args");
    assert!(validate_operation(&parse_operation(&ok)).is_ok());
}

#[test]
fn a_registered_or_transcript_operation_needs_no_argv_rules() {
    let mut registered = operation();
    registered["implementation"] =
        json!({"kind": "registered", "tool": "read_file", "digest": "a".repeat(64)});
    assert!(validate_operation(&parse_operation(&registered)).is_ok());

    let mut transcript = operation();
    transcript["implementation"] = json!({"kind": "transcript"});
    assert!(validate_operation(&parse_operation(&transcript)).is_ok());
}

#[test]
fn a_call_builds_argv_from_constants_and_validated_scalars() {
    let root = policy_root();
    std::fs::create_dir(root.path().join("workspace")).unwrap();
    let jail = WorkspaceJail::new(JailOptions::new(root.path().join("workspace"))).unwrap();
    let resolved = resolve(&root, &manifest()).unwrap();
    let status = &resolved.operations["status"];

    assert_eq!(
        command_argv(status, &json!({}), &jail).unwrap(),
        ["/usr/bin/git", "status", "--porcelain"]
    );
    // The schema is closed, so an extra argument is refused rather than
    // appended.
    assert!(command_argv(status, &json!({"args": ["push"]}), &jail).is_err());
    assert_eq!(
        validate_input(status, &json!({"args": []}))
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );

    let mut with_path = operation();
    with_path["parameters"]["properties"]["path"] = json!({"type": "string"});
    with_path["parameters"]["properties"]["depth"] = json!({"type": "integer"});
    with_path["parameters"]["required"] = json!(["path", "depth"]);
    with_path["implementation"]["argv"] =
        json!(["log", {"input": "depth"}, {"input": "path", "workspacePath": true}]);
    let with_path = parse_operation(&with_path);
    let argv = command_argv(&with_path, &json!({"path": "src", "depth": 3}), &jail).unwrap();
    assert_eq!(argv[1], "log");
    assert_eq!(argv[2], "3");
    assert!(argv[3].ends_with("/src"));
    // The jail clamps rather than refusing, so an escaping path lands inside
    // the workspace instead of reaching the parent directory.
    let clamped = command_argv(&with_path, &json!({"path": "../out", "depth": 1}), &jail).unwrap();
    assert!(clamped[3].starts_with(&jail.root().to_string_lossy().into_owned()));
    // A non-string where a workspace path was promised is refused by the
    // schema before argv is built at all.
    assert!(command_argv(&with_path, &json!({"path": 7, "depth": 1}), &jail).is_err());

    let mut transcript = operation();
    transcript["implementation"] = json!({"kind": "transcript"});
    assert!(
        message_of(&command_argv(
            &parse_operation(&transcript),
            &json!({}),
            &jail
        ))
        .contains("Not a command")
    );
}

#[test]
fn a_permission_can_only_be_narrowed() {
    use ToolPermission::{Allow, Ask, Deny};
    assert_eq!(narrow_permission(Ask, Allow), Ask);
    assert_eq!(narrow_permission(Deny, Allow), Deny);
    assert_eq!(narrow_permission(Allow, Deny), Deny);
    assert_eq!(narrow_permission(Allow, Ask), Ask);
    assert_eq!(narrow_permission(Allow, Allow), Allow);
}

fn network(mode: NetworkMode, allow: &[&str], hosts: &[&str], dns: &[&str]) -> ContainerNetwork {
    ContainerNetwork {
        mode,
        allow: allow.iter().map(|v| (*v).to_owned()).collect(),
        hosts: hosts.iter().map(|v| (*v).to_owned()).collect(),
        dns: dns.iter().map(|v| (*v).to_owned()).collect(),
    }
}

#[test]
fn a_mode_that_reaches_nothing_may_not_carry_entries() {
    for mode in [NetworkMode::None, NetworkMode::Open] {
        for asked in [
            network(mode, &["10.0.0.0/8"], &[], &[]),
            network(mode, &[], &["example.com"], &[]),
            network(mode, &[], &[], &["1.1.1.1"]),
        ] {
            let error = assert_container_network(&asked, "pentest").unwrap_err();
            assert!(error.message.contains("would have no effect"));
            assert_eq!(error.details["agentId"], json!("pentest"));
        }
        assert!(assert_container_network(&network(mode, &[], &[], &[]), "a").is_ok());
    }
}

#[test]
fn an_allowlist_needs_entries_of_exactly_one_kind() {
    let empty =
        assert_container_network(&network(NetworkMode::Allowlist, &[], &[], &[]), "a").unwrap_err();
    assert!(empty.message.contains("reaches nothing"));

    let both = assert_container_network(
        &network(
            NetworkMode::Allowlist,
            &["10.0.0.0/8"],
            &["example.com"],
            &[],
        ),
        "a",
    )
    .unwrap_err();
    assert!(both.message.contains("Choose one"));
}

#[test]
fn an_allowlist_entry_must_be_a_cidr_and_a_host_an_exact_name() {
    for entry in [
        "example.com",
        "10.0.0.1",
        "not a cidr",
        "10.0.0.0/64",
        "10.0.0.0/+8",
    ] {
        let error = assert_container_network(
            &network(NetworkMode::Allowlist, &[entry], &[], &["1.1.1.1"]),
            "a",
        )
        .unwrap_err();
        assert!(error.message.contains("not a CIDR"), "{entry}");
        assert_eq!(error.details["entry"], json!(entry));
    }
    for host in ["*.example.com", ".example.com", "example.com.", "", "a b"] {
        let error =
            assert_container_network(&network(NetworkMode::Allowlist, &[], &[host], &[]), "a")
                .unwrap_err();
        assert!(error.message.contains("exact DNS name"), "{host}");
        assert_eq!(error.details["host"], json!(host));
    }
    let long = "a".repeat(254);
    assert!(
        assert_container_network(&network(NetworkMode::Allowlist, &[], &[&long], &[]), "a")
            .is_err()
    );
    assert!(
        assert_container_network(
            &network(NetworkMode::Allowlist, &[], &["deb.debian.org"], &[]),
            "a"
        )
        .is_ok()
    );
}

#[test]
fn a_resolver_must_be_an_ip_literal_and_a_cidr_list_needs_one() {
    let named = assert_container_network(
        &network(
            NetworkMode::Allowlist,
            &["10.0.0.0/8"],
            &[],
            &["dns.example.com"],
        ),
        "a",
    )
    .unwrap_err();
    assert!(named.message.contains("IP literal"));
    assert_eq!(named.details["resolver"], json!("dns.example.com"));

    let unresolvable = assert_container_network(
        &network(NetworkMode::Allowlist, &["10.0.0.0/8"], &[], &[]),
        "a",
    )
    .unwrap_err();
    assert!(unresolvable.message.contains("names no DNS resolver"));

    assert!(
        assert_container_network(
            &network(
                NetworkMode::Allowlist,
                &["10.0.0.0/8", "2001:db8::/32"],
                &[],
                &["1.1.1.1"]
            ),
            "a"
        )
        .is_ok()
    );
}
