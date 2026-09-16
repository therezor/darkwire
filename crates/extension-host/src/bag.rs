//! The recorder: everything one extension turned out to contribute, held by
//! the host rather than pushed into a registry.
//!
//! This is the load-bearing decision in the package, and it survived the move
//! out of process unchanged — because the three things it buys were never
//! about being in-process:
//!
//!  - **Unload is exact.** The host holds the bag the activation filled, so
//!    removing an extension is removing exactly what that bag named. Nothing
//!    has to be diffed and nothing can be missed.
//!  - **A partial activation installs nothing.** An extension whose
//!    `tools/list` answers and whose `darkwire/commands/list` dies leaves no
//!    trace, because the bag is discarded whole. The alternative — registering
//!    each kind as it arrives — leaves four tools registered by an extension
//!    that is not running.
//!  - **Nothing an extension holds outlives it.** There is no handle to take
//!    back, because none was ever given out.
//!
//! Two rules are enforced on the way in, and both are **warnings on a row,
//! never refusals of the extension**. An extension whose fifth tool is misnamed
//! should install the other four and say so, because the alternative is an
//! operator with a working extension that vanished and a log line to find.
//!
//! **One namespace, checked once.** Every id an extension contributes is
//! `<id>` or `<id>-<suffix>`. A channel id becomes a session-key prefix, a
//! provider id becomes a `providers.<id>.type`, a command id becomes what an
//! operator types after a slash — three registries and one character class, so
//! two extensions cannot silently fight over a name and an operator reading any
//! of the three can tell whose it is. Tool names are the exception in *spelling*
//! only: they have their own character class, so a tool goes through the MCP
//! bridge's flattener and comes out `ext_<id>_<name>` — the same rule,
//! transliterated.
//!
//! **`contributes` has to mean something.** A registration whose kind the
//! manifest never declared is dropped. That keeps the approval screen honest: an
//! operator who approved "channels and commands" is not surprised by a tool. It
//! is *not* a security boundary and this file does not pretend otherwise — the
//! extension is a process with the operator's own privileges and could open a
//! socket without asking anyone. What it stops is an honest mistake becoming an
//! invisible one.

use std::collections::HashSet;
use std::sync::Arc;

use darkwire_agent::ContextContributor;
use darkwire_channels::ChannelFactory;
use darkwire_protocol::{ExtensionCommand, ExtensionContribution, ExtensionManifest};
use darkwire_providers::ProviderSpec;
use darkwire_tools::AnyTool;

/// Everything one extension contributed, and what was refused on the way.
#[derive(Default)]
pub struct Registration {
    /// Bridged tools, already named `ext_<id>_<tool>`.
    pub tools: Vec<AnyTool>,
    /// Channel factories, each building a channel over the RPC connection.
    pub channels: Vec<ChannelFactory>,
    /// Provider types, as data. The host owns the wire.
    pub providers: Vec<ProviderSpec>,
    /// System-prompt contributors.
    pub contributors: Vec<Arc<dyn ContextContributor>>,
    /// Slash commands, as the wire describes them.
    pub commands: Vec<ExtensionCommand>,
    /// Ids and kinds that were refused, phrased for the operator.
    pub warnings: Vec<String>,
}

impl std::fmt::Debug for Registration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registration")
            .field("tools", &self.tools.len())
            .field("channels", &self.channels.len())
            .field("providers", &self.providers.len())
            .field("contributors", &self.contributors.len())
            .field("commands", &self.commands.len())
            .field("warnings", &self.warnings)
            .finish()
    }
}

impl Registration {
    /// The tool names, sorted, for the status row.
    pub fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .tools
            .iter()
            .map(|tool| tool.definition().name.clone())
            .collect();
        names.sort();
        names
    }

    /// The channel ids, sorted.
    pub fn channel_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .channels
            .iter()
            .map(|factory| factory.id().to_owned())
            .collect();
        ids.sort();
        ids
    }

    /// The provider ids, sorted.
    pub fn provider_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.providers.iter().map(|spec| spec.id.clone()).collect();
        ids.sort();
        ids
    }

    /// The command ids, sorted.
    pub fn command_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .commands
            .iter()
            .map(|command| command.id.clone())
            .collect();
        ids.sort();
        ids
    }
}

/// Collects one extension's registrations, applying the two rules.
///
/// A struct rather than a function because the host fills it over several round
/// trips — one per contribution kind — and reads it once at the end.
pub struct RegistrationBag {
    id: String,
    declared: HashSet<ExtensionContribution>,
    registration: Registration,
}

impl std::fmt::Debug for RegistrationBag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistrationBag")
            .field("id", &self.id)
            .field("registration", &self.registration)
            .finish_non_exhaustive()
    }
}

impl RegistrationBag {
    /// A bag for one extension, closed over what its manifest declares.
    pub fn new(manifest: &ExtensionManifest) -> RegistrationBag {
        RegistrationBag {
            id: manifest.id.clone(),
            declared: manifest.contributes.iter().copied().collect(),
            registration: Registration::default(),
        }
    }

    /// Whether the manifest declared this kind at all.
    ///
    /// Public because the host reads it *before* spending a round trip: there
    /// is no point asking an extension for its commands when the manifest never
    /// said it had any, and the probe that produces the mirror-image warning is
    /// only run on kinds that were declared.
    pub fn declares(&self, kind: ExtensionContribution) -> bool {
        self.declared.contains(&kind)
    }

    /// Every declared kind, in the order the wire lists them.
    pub fn declared(&self) -> Vec<ExtensionContribution> {
        [
            ExtensionContribution::Tools,
            ExtensionContribution::Channels,
            ExtensionContribution::Providers,
            ExtensionContribution::Context,
            ExtensionContribution::Commands,
        ]
        .into_iter()
        .filter(|kind| self.declares(*kind))
        .collect()
    }

    /// Records a sentence for the row without refusing anything.
    pub fn warn(&mut self, warning: impl Into<String>) {
        self.registration.warnings.push(warning.into());
    }

    /// A tool, already bridged and already namespaced by the flattener.
    pub fn add_tool(&mut self, tool: AnyTool) {
        if !self.allows(ExtensionContribution::Tools) {
            return;
        }
        self.registration.tools.push(tool);
    }

    /// A channel factory. Its id has to be namespaced.
    pub fn add_channel(&mut self, factory: ChannelFactory) {
        if !self.allows(ExtensionContribution::Channels) {
            return;
        }
        if !self.namespaced(factory.id(), "channel") {
            return;
        }
        self.registration.channels.push(factory);
    }

    /// A provider type. Its id has to be namespaced.
    pub fn add_provider(&mut self, spec: ProviderSpec) {
        if !self.allows(ExtensionContribution::Providers) {
            return;
        }
        if !self.namespaced(&spec.id, "provider") {
            return;
        }
        self.registration.providers.push(spec);
    }

    /// A prompt contributor. It has no id, so only the kind is checked.
    pub fn add_contributor(&mut self, contributor: Arc<dyn ContextContributor>) {
        if !self.allows(ExtensionContribution::Context) {
            return;
        }
        self.registration.contributors.push(contributor);
    }

    /// A slash command. Its id has to be namespaced.
    pub fn add_command(&mut self, command: ExtensionCommand) {
        if !self.allows(ExtensionContribution::Commands) {
            return;
        }
        if !self.namespaced(&command.id, "command") {
            return;
        }
        self.registration.commands.push(command);
    }

    /// The bag, ready to be held or dropped whole.
    pub fn finish(self) -> Registration {
        self.registration
    }

    fn allows(&mut self, kind: ExtensionContribution) -> bool {
        if self.declared.contains(&kind) {
            return true;
        }
        let name = kind_name(kind);
        self.warn(format!(
            "Registered {name}, which the manifest's \"contributes\" does not declare. \
             Add \"{name}\" to it and re-approve."
        ));
        false
    }

    fn namespaced(&mut self, id: &str, kind: &str) -> bool {
        if id == self.id || id.starts_with(&format!("{}-", self.id)) {
            return true;
        }
        let own = self.id.clone();
        self.warn(format!(
            "The {kind} \"{id}\" is not namespaced to this extension. \
             It has to be \"{own}\" or start with \"{own}-\"."
        ));
        false
    }
}

/// The wire spelling of a contribution kind, for a sentence an operator reads.
pub fn kind_name(kind: ExtensionContribution) -> &'static str {
    match kind {
        ExtensionContribution::Tools => "tools",
        ExtensionContribution::Channels => "channels",
        ExtensionContribution::Providers => "providers",
        ExtensionContribution::Context => "context",
        ExtensionContribution::Commands => "commands",
    }
}
