//! The crate that has to be right.
//!
//! Everything that judges an agent-supplied path, host, command or manifest lives
//! here and nowhere else: the workspace jail, the exec guard, the pinned-address
//! fetch, the credential vault, tool-output fencing and the approval stores.
//! Isolated so the whole surface is one directory rather than a grep. Coverage
//! bar 95/95, because an untested branch here is a bypass, not a bug.
//!
//! The guards, and the class of attack each one closes:
//!
//!  - [`WorkspaceJail`] — path traversal and symlink escape, by normalising every
//!    input into the workspace root lexically and then canonicalising through
//!    the filesystem before deciding containment.
//!  - [`wrap_tool_output`] — prompt injection, by fencing untrusted output inside
//!    a per-turn random delimiter. Detection is non-destructive: content passes
//!    through byte-for-byte and a notice raises the badge.
//!  - [`guarded_fetch`] — SSRF and DNS rebinding, by pinning the addresses that
//!    validation resolved into the client that connects, and re-validating every
//!    redirect hop.
//!  - [`guard_exec`] — command injection, by taking an argv, jailing every
//!    path-shaped argument, and refusing the shell invocations that would put a
//!    parser back in the middle.
//!  - [`CredentialVault`] — credential theft at rest, with AES-256-GCM under a
//!    key from the OS keychain or a `0600` keyfile.
//!  - [`extension_digest`] / [`ExtensionStore`] — code loading itself into the
//!    host, by hashing every byte of an install directory and refusing to load
//!    one whose digest is not the one an operator approved.
//!
//! The layering that makes the isolation true is enforced by Cargo:
//! `darkwire-core` lists no HTTP client and spawns no process, so there is no
//! way to reach either without coming through here first.
#![forbid(unsafe_code)]

pub mod allow;
pub mod egress;
pub mod environment;
pub mod exec_guard;
pub mod exec_rules;
pub mod extension;
pub mod extension_store;
pub mod fetch;
pub mod ip;
pub mod jail;
pub mod keychain;
pub mod nonce;
pub mod policy_store;
pub mod random;
pub mod vault;

#[cfg(feature = "testkit")]
pub mod testkit;

pub use allow::{AllowEntry, AllowList, parse_allow_entry};
pub use environment::{
    BUILTIN_TOOL_NAMES, assert_environment_network, assert_environment_policy,
    assert_gateway_compatible, assert_slug, invalid, manifest_hash, parse_environment, weakened_in,
};
pub use exec_guard::{
    ExecGuardOptions, ExecPlan, OutputCap, OutputCapResult, SHELL_BINARIES, binary_name, guard_exec,
};
pub use exec_rules::{
    ExecRules, ExecVerdict, ParsedRule, assert_exec_rules, assert_standing_rule, exec_verdict,
    invocation_digest, is_shell, parse_exec_rule,
};
pub use extension::{
    EXTENSION_MANIFEST_FILE, MAX_EXTENSION_BYTES, MAX_EXTENSION_FILES, assert_extension_policy,
    extension_digest, parse_extension, read_extension_manifest,
};
pub use extension_store::{ExtensionResolution, ExtensionResolutionState, ExtensionStore};
pub use fetch::{
    DnsResolver, EgressAllow, GuardedFetchOptions, GuardedFetchResult, GuardedResponse,
    HickoryResolver, NetworkPolicy, PinnedTarget, guarded_fetch, validate_target,
};
pub use ip::{
    AddressCategory, AddressRange, BLOCKED_RANGES, IpFamily, ParsedCidr, ParsedIp, cidr_contains,
    classify_address, parse_cidr, parse_ip_literal,
};
pub use jail::{
    JailAccept, JailCheck, JailOptions, JailRejection, JailResolver, PathShape, SingleJail,
    WorkspaceJail, path_shapes, single_jail,
};
pub use keychain::{
    CommandResult, CommandRunner, KeychainOptions, KeychainStore, Platform, SystemCommandRunner,
};
pub use nonce::{
    InjectionFinding, InjectionSignal, TOOL_OUTPUT_NONCE_BYTES, WrapToolOutputOptions,
    WrappedToolOutput, create_tool_output_nonce, describe_injection_findings,
    detect_prompt_injection, tool_output_policy, tool_output_tag, wrap_tool_output,
};
pub use policy_store::{EnvironmentListing, InstalledEnvironment, PolicyStore};
pub use random::{OsRandom, RandomSource};
pub use vault::{
    CredentialVault, KeyFileStore, KeyStore, ResolvedVaultKey, VAULT_KEY_BYTES, resolve_vault_key,
};
