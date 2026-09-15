//! Finding installed extensions, and deciding what this build can run.
//!
//! Discovery is two sources and one precedence rule: the directory scan under
//! `<root>/extensions`, plus whatever `extensions.load` names. An explicit path
//! **wins** over an install-directory entry with the same id, because it is the
//! more specific statement — a scan is what happens to be there and a `load`
//! entry is something an operator wrote down. Which is also why `allowOverride`
//! does not govern that: it governs two *discovered* extensions claiming one id,
//! which the directory scan makes impossible (an id is a directory name) and
//! which only `load` can produce.
//!
//! The other half is the version gate. `ghostai.extension/1` named a JavaScript
//! module that a host loaded into its own process; this host spawns a child and
//! talks to it over a pipe, and no amount of care makes the first contract into
//! the second. So a v1 bundle is **refused with a sentence**, not ignored and
//! not crashed on — and refused *late*, after the manifest has produced an id
//! and a label, so the operator gets a row naming the extension rather than a
//! log line naming a file.

use std::collections::BTreeMap;
use std::path::Path;

use ghostai_protocol::{ExtensionSchemaVersion, ExtensionsConfig};
use ghostai_security::{EXTENSION_MANIFEST_FILE, ExtensionResolution, ExtensionStore};

/// What an operator reads when a v1 bundle reaches this build.
///
/// Phrased as a property of the *bundle* rather than of the host, because that
/// is the half they can act on: the fix is a rebuilt extension, and no setting
/// here will make this one load.
pub const V1_UNSUPPORTED: &str = concat!(
    "This extension is a \"ghostai.extension/1\" bundle, which ran as JavaScript\n",
    "  inside the server process. This build runs an extension as a separate\n",
    "  program and talks to it over a pipe, so it cannot load one. Rebuild it\n",
    "  against \"ghostai.extension/2\", which replaces \"entry\" with \"command\"."
);

/// The `schema` value an install directory's manifest carries, read raw.
///
/// Needed because the resolution the store hands back has no manifest at all
/// when the policy refused one — and "the policy refused it" and "this build
/// cannot run this version" are different sentences with different fixes. The
/// file is read as loose JSON rather than parsed into the manifest type, since
/// the whole point is to say something useful about a manifest that did not
/// parse.
pub fn schema_on_disk(dir: &Path) -> Option<ExtensionSchemaVersion> {
    let bytes = std::fs::read(dir.join(EXTENSION_MANIFEST_FILE)).ok()?;
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&bytes).ok()?;
    match value.get("schema")?.as_str()? {
        "ghostai.extension/1" => Some(ExtensionSchemaVersion::V1),
        "ghostai.extension/2" => Some(ExtensionSchemaVersion::V2),
        _ => None,
    }
}

/// Whether this install directory holds something this build refuses on sight.
///
/// Separate from the store's own resolution because the store is version-blind
/// by design: it answers "are these the bytes that were approved?", and an
/// approved v1 bundle is still an approved v1 bundle. Running it is this
/// layer's decision.
pub fn refuses_version(resolution: &ExtensionResolution) -> bool {
    let version = resolution
        .manifest
        .as_ref()
        .map(|manifest| manifest.schema)
        .or_else(|| schema_on_disk(&resolution.dir));
    version == Some(ExtensionSchemaVersion::V1)
}

/// Every extension the install knows about, by id.
///
/// Sorted, because the status list is read by a panel and by
/// `ghostai extension list`, and an order that depended on the filesystem would
/// make two machines with the same extensions disagree about the order they are
/// shown in.
///
/// A path in `extensions.load` that is not a directory is warned about and
/// skipped rather than reported as a broken extension: there is no id to hang a
/// row on, and a typo in a path is not an extension that is failing.
pub fn discover(
    store: &ExtensionStore,
    config: &ExtensionsConfig,
) -> BTreeMap<String, ExtensionResolution> {
    let mut found = BTreeMap::new();

    for id in store.installed_ids() {
        match store.resolve(&id) {
            Ok(resolution) => {
                found.insert(id, resolution);
            }
            Err(error) => tracing::warn!(
                target: "extension",
                extension = %id,
                error = %error.message,
                "an installed extension could not be resolved"
            ),
        }
    }

    for path in &config.load {
        let resolution = match store.resolve_path(Path::new(path)) {
            Ok(Some(resolution)) => resolution,
            Ok(None) => {
                tracing::warn!(
                    target: "extension",
                    path,
                    "extensions.load names a path that is not a directory"
                );
                continue;
            }
            Err(error) => {
                tracing::warn!(
                    target: "extension",
                    path,
                    error = %error.message,
                    "extensions.load names a path that could not be resolved"
                );
                continue;
            }
        };
        if let Some(existing) = found.get(&resolution.id)
            && !config.allow_override
        {
            tracing::warn!(
                target: "extension",
                extension = %resolution.id,
                path,
                installed = %existing.dir.display(),
                "an explicitly loaded extension shadows an installed one of the same id"
            );
        }
        found.insert(resolution.id.clone(), resolution);
    }

    found
}

/// One extension's block of `config.extensions.settings`.
pub fn settings_for(config: &ExtensionsConfig, id: &str) -> ghostai_protocol::json::Object {
    config.settings.get(id).cloned().unwrap_or_default()
}
