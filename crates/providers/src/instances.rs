//! Configured endpoints, as distinct from the providers they speak to.
//!
//! `registry` describes *types*: what Ollama is, which wire OpenAI speaks,
//! which environment variable holds a key. This module describes what an
//! operator actually configured: a list of instances, each naming a type. The
//! split is the whole point. Two Ollama servers are two instances of one type,
//! and the settings UI needs both lists at once: the catalogue to add from,
//! and the configured endpoints to edit.
//!
//! Nothing here narrows `ProvidersConfig` to a known-ids map. An instance id
//! is an operator's label; the registry has no opinion about it.

use ghostai_protocol::{ProviderConfig, ProviderInstanceInfo, ProvidersConfig};

use crate::registry::{
    GatewayHints, PROVIDERS, ProviderSpec, find_gateway, find_provider, find_provider_by_model,
};

/// One configured endpoint, with its type resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderInstance {
    /// The config key. Also the vault key its credential is stored under.
    pub id: String,
    /// The type it is an instance of.
    pub spec: ProviderSpec,
    /// What the operator wrote.
    pub config: ProviderConfig,
}

/// The name to show for an instance: its label, or the type's display name.
pub fn instance_label(instance: &ProviderInstance) -> String {
    let label = instance.config.label.trim();
    if label.is_empty() {
        instance.spec.display_name.clone()
    } else {
        label.to_owned()
    }
}

/// An instance as the settings UI sees it.
///
/// `api_base` is the *effective* endpoint, with the type's default folded in,
/// so a panel can show which URL a turn would actually reach without opening
/// a connection to find out. `credentials_present` is the one thing the
/// config cannot know and the caller supplies: a boolean, never the value.
pub fn describe_instance(
    instance: &ProviderInstance,
    credentials_present: bool,
) -> ProviderInstanceInfo {
    let spec = &instance.spec;
    let configured = instance
        .config
        .api_base
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    ProviderInstanceInfo {
        id: instance.id.clone(),
        kind: spec.id.clone(),
        display_name: instance_label(instance),
        api_base: if configured.is_empty() {
            spec.default_api_base.clone().unwrap_or_default()
        } else {
            configured.to_owned()
        },
        is_local: spec.is_local,
        is_gateway: spec.is_gateway,
        is_o_auth: spec.is_o_auth,
        env_key: spec.env_key.clone(),
        enabled: instance.config.enabled,
        supports_model_listing: spec.supports_model_listing,
        credentials_present,
    }
}

/// Every entry whose `type` names a real provider, in config order.
///
/// Insertion order is load-bearing rather than incidental: it is the order the
/// settings panel lists instances in, and the tie-break [`resolve_instance`]
/// uses when two instances are equally good candidates. The config map keeps
/// it, so what an operator sees in their file is what resolution walks.
///
/// An entry naming an unknown type is skipped rather than refused. It is a
/// typo in one instance, and refusing to list the other nine, or refusing to
/// boot, would make a single bad character take the whole install down.
pub fn list_instances(
    providers: &ProvidersConfig,
    specs: &[ProviderSpec],
) -> Vec<ProviderInstance> {
    providers
        .iter()
        .filter_map(|(id, config)| {
            find_provider(&config.kind, specs).map(|spec| ProviderInstance {
                id: id.clone(),
                spec: spec.clone(),
                config: config.clone(),
            })
        })
        .collect()
}

/// The instance `id` names, if it exists and its type does.
pub fn find_instance(
    providers: &ProvidersConfig,
    id: &str,
    specs: &[ProviderSpec],
) -> Option<ProviderInstance> {
    let config = providers.get(id)?;
    let spec = find_provider(&config.kind, specs)?;
    Some(ProviderInstance {
        id: id.to_owned(),
        spec: spec.clone(),
        config: config.clone(),
    })
}

/// An instance for a type that has none configured.
///
/// `ghostai chat --provider ollama` has to work on a machine with no config
/// file, and it did before instances existed. Rather than special-casing that
/// path everywhere downstream, resolution synthesises the instance the old
/// code was effectively using: id = the type, so even its vault lookup lands
/// where a pre-instance install stored its key.
fn synthetic_instance(spec: &ProviderSpec) -> ProviderInstance {
    ProviderInstance {
        id: spec.id.clone(),
        spec: spec.clone(),
        config: ProviderConfig {
            kind: spec.id.clone(),
            label: String::new(),
            api_base: None,
            extra_headers: indexmap::IndexMap::new(),
            models: Vec::new(),
            enabled: true,
        },
    }
}

/// A `-2`, `-3`, ... suffix is added only when the bare type is taken.
pub fn next_instance_id<'a>(kind: &str, taken: impl IntoIterator<Item = &'a str>) -> String {
    let used: std::collections::HashSet<&str> = taken.into_iter().collect();
    if !used.contains(kind) {
        return kind.to_owned();
    }
    let mut n = 2u64;
    loop {
        let candidate = format!("{kind}-{n}");
        if !used.contains(candidate.as_str()) {
            return candidate;
        }
        n += 1;
    }
}

/// What [`resolve_instance`] has to go on.
pub struct ResolveInstanceOptions<'a> {
    /// The configured instances.
    pub providers: &'a ProvidersConfig,
    /// An instance id, a bare provider type, or `auto`.
    pub provider: Option<&'a str>,
    /// The model, for keyword matching under `auto`.
    pub model: Option<&'a str>,
    /// Consulted only to break a tie under `auto`; never to reject an
    /// instance.
    pub has_credential: Option<&'a dyn Fn(&str) -> bool>,
    /// The provider types resolution may see.
    ///
    /// Defaults to the built-in table. The composition root passes the
    /// built-ins plus whatever extensions contributed, which is what makes
    /// `providers.<id>.type` able to name a provider this build did not ship.
    pub specs: Option<&'a [ProviderSpec]>,
}

impl std::fmt::Debug for ResolveInstanceOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolveInstanceOptions")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

/// The instance to use, in the one order the whole system agrees on.
///
/// 1. An instance id that exists: the operator named this endpoint.
/// 2. A provider type: its first enabled instance, or a synthetic one.
/// 3. `auto`: the model's implied type, then a gateway identified by its base
///    URL, then the first instance holding a credential, then the first.
///
/// Returns `None` rather than guessing when none of those answer, so the
/// caller reports "not configured" instead of sending a request to an
/// endpoint nobody chose. That is `resolve_provider`'s rule, kept.
///
/// Disabled instances are invisible to every step, including an explicit id:
/// a switch that still resolved would not be a switch.
pub fn resolve_instance(options: &ResolveInstanceOptions<'_>) -> Option<ProviderInstance> {
    let specs: &[ProviderSpec] = options.specs.unwrap_or(&PROVIDERS);
    let enabled: Vec<ProviderInstance> = list_instances(options.providers, specs)
        .into_iter()
        .filter(|instance| instance.config.enabled)
        .collect();

    if let Some(named) = options
        .provider
        .filter(|name| !name.is_empty() && *name != "auto")
    {
        if let Some(exact) = enabled.iter().find(|instance| instance.id == named) {
            return Some(exact.clone());
        }
        // A name that is neither an instance nor a type is a typo, and falling
        // through to `auto` would silently answer with some other endpoint.
        let spec = find_provider(named, specs)?;
        return Some(
            enabled
                .iter()
                .find(|instance| instance.spec.id == spec.id)
                .cloned()
                .unwrap_or_else(|| synthetic_instance(spec)),
        );
    }

    if enabled.is_empty() {
        return None;
    }

    if let Some(by_model) = options
        .model
        .and_then(|model| find_provider_by_model(model, specs))
        && let Some(found) = enabled
            .iter()
            .find(|instance| instance.spec.id == by_model.id)
    {
        return Some(found.clone());
    }

    if let Some(gateway) = enabled.iter().find(|instance| {
        let Some(api_base) = instance
            .config
            .api_base
            .as_deref()
            .filter(|base| !base.is_empty())
        else {
            return false;
        };
        find_gateway(&GatewayHints {
            provider_id: Some(&instance.spec.id),
            api_key: None,
            api_base: Some(api_base),
        })
        .is_some_and(|found| found.id == instance.spec.id)
    }) {
        return Some(gateway.clone());
    }

    let credentialed = options
        .has_credential
        .and_then(|has_credential| enabled.iter().find(|instance| has_credential(&instance.id)));
    credentialed.or(enabled.first()).cloned()
}
