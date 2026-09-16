//! The seams `darkwire serve` opens for the end-to-end suite, and nothing else.
//!
//! **Two independent switches, and both are needed**, the same rule the hook
//! routes follow: the `test-hooks` cargo feature decides whether this module is
//! compiled at all, and `DARKWIRE_TEST_HOOKS=1` decides whether anything in it is
//! reached. A release artefact is built without the feature, so the environment
//! variable finds nothing; the binary CI hands to Playwright *was* built with it
//! and still behaves like a shipping one until the harness says otherwise.
//!
//! Three seams, and each of them exists because the browser suite cannot use the
//! real thing without touching something that does not belong to it:
//!
//!  - **The vault skips the keychain.** Resolving a vault key writes an entry to
//!    the operator's login keychain the first time it runs, and on macOS that is
//!    a prompt a test runner cannot answer — the suite's first act would be to
//!    hang. The key file under the temporary home is the whole store instead, and
//!    it is deleted with the home. This is the *only* substitution: the vault
//!    itself, its envelope and its round trip are the shipping ones.
//!  - **The password hasher is a comparison.** argon2id is ~50 ms and 19 MiB per
//!    call by design; a suite that logs in once per test pays that twice per
//!    test for a property `auth_store` already proves at a far higher bar.
//!  - **`e2e_wait` exists.** Two things a browser has to be able to stand still
//!    in — Stop reaching a tool that is mid-call, and a reload rebuilding a turn
//!    that has not finished — need a tool that is reliably still running a
//!    moment after it started. Sleeping on a real binary would make both depend
//!    on how this machine's `sleep` resolves; waiting on the turn's own token
//!    depends on nothing.

use std::sync::Arc;

use darkwire_core::paths::WirePaths;
use darkwire_core::{Result, WireError};
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use darkwire_security::{CredentialVault, KeyFileStore, KeyStore, OsRandom, resolve_vault_key};
use darkwire_server::auth_store::PasswordHasher;
use darkwire_tools::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
};
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;

/// The environment variable that arms the seams at run time.
pub const TEST_HOOKS_ENV: &str = "DARKWIRE_TEST_HOOKS";

/// Whether the environment armed them.
///
/// Read from the process environment rather than from the injected [`Env`], and
/// deliberately: the same lookup the hook routes make, so a build can never be
/// in the state where the routes answer and the seams do not.
///
/// [`Env`]: crate::i18n::Env
pub fn armed() -> bool {
    std::env::var(TEST_HOOKS_ENV).is_ok_and(|value| value == "1")
}

/// The vault, with the keychain left out of the key resolution.
///
/// `None` when the seams are not armed, which is what leaves the real
/// [`VaultChoice::Default`] in place; `None` also when the key file cannot be
/// written, which the caller treats the way it treats a vault that will not
/// open.
///
/// [`VaultChoice::Default`]: darkwire_runtime::VaultChoice::Default
pub fn vault(paths: &WirePaths) -> Option<Arc<Mutex<CredentialVault>>> {
    if !armed() {
        return None;
    }
    let random = Arc::new(OsRandom);
    let key_file = KeyFileStore::new(paths.key_file.clone());
    let stores: [&dyn KeyStore; 1] = [&key_file];
    let resolved = resolve_vault_key(&stores, random.as_ref()).ok()?;
    let opened = CredentialVault::open(&paths.vault_file, &resolved.key, random).ok()?;
    Some(Arc::new(Mutex::new(opened)))
}

/// A digest that is the password with a prefix, and the comparison to match.
#[derive(Debug, Clone, Copy)]
struct FakeHasher;

impl PasswordHasher for FakeHasher {
    fn hash(&self, password: &str) -> Result<String> {
        Ok(format!("fake:{password}"))
    }

    fn verify(&self, hash: &str, password: &str) -> bool {
        hash == format!("fake:{password}")
    }
}

/// The hasher, or `None` to leave argon2id in place.
pub fn hasher() -> Option<Arc<dyn PasswordHasher>> {
    armed().then(|| Arc::new(FakeHasher) as Arc<dyn PasswordHasher>)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitArgs {
    #[schemars(range(min = 0), description = "How long to wait, in milliseconds.")]
    ms: u64,
}

struct Wait;

impl ToolHandler for Wait {
    type Args = WaitArgs;

    fn execute<'a>(
        &'a self,
        args: WaitArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            let slept = tokio::time::sleep(std::time::Duration::from_millis(args.ms));
            tokio::select! {
                () = slept => Ok(ToolOutput::text(format!("waited {}ms", args.ms))),
                () = ctx.token.cancelled() => Err(WireError::aborted("e2e_wait")),
            }
        })
    }
}

/// The waiting tool, or `None` when the seams are not armed.
///
/// `risk: safe` on purpose: the approval prompt is the subject of its own spec,
/// and a Stop test that had to approve a tool first would be asserting two
/// things and failing for either.
pub fn wait_tool() -> Option<AnyTool> {
    if !armed() {
        return None;
    }
    let spec = ToolSpec {
        risk: ToolRisk::Safe,
        annotations: Some(ToolAnnotations {
            title: Some("Wait".to_owned()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            ..ToolAnnotations::default()
        }),
        ..ToolSpec::new(
            "e2e_wait",
            "Wait for a while, then report that the wait finished.",
        )
    };
    TypedTool::new(spec, Wait)
        .ok()
        .map(|tool| Arc::new(tool) as AnyTool)
}
