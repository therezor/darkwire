//! The two rules, and the fact that neither of them refuses an extension.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use ghostai_agent::ContextContributor;
use ghostai_channels::{Channel, ChannelContext, ChannelFactory};
use ghostai_core::Result;
use ghostai_core::message_bus::OutboundMessage;
use ghostai_extension_host::{RegistrationBag, kind_name};
use ghostai_protocol::{ExtensionCommand, ExtensionContribution, ExtensionManifest};
use ghostai_providers::{ProviderSpec, WireProtocol};
use serde_json::json;

fn manifest(contributes: &[&str]) -> ExtensionManifest {
    serde_json::from_value(json!({
        "schema": "ghostai.extension/2",
        "id": "slack",
        "command": ["node", "index.mjs"],
        "contributes": contributes,
    }))
    .unwrap()
}

struct Nothing(String);

impl Channel for Nothing {
    fn id(&self) -> &str {
        &self.0
    }
    fn send(&self, _message: OutboundMessage) -> ghostai_channels::BoxFuture<'_, Result<()>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

fn factory(id: &str) -> ChannelFactory {
    let own = id.to_owned();
    ChannelFactory::new(
        id,
        Arc::new(move |_context: ChannelContext| {
            Ok(Arc::new(Nothing(own.clone())) as Arc<dyn Channel>)
        }),
    )
}

struct Quiet;

impl ContextContributor for Quiet {
    fn name(&self) -> &'static str {
        "slack"
    }
}

fn command(id: &str) -> ExtensionCommand {
    ExtensionCommand {
        id: id.to_owned(),
        extension_id: "slack".to_owned(),
        description: String::new(),
        args_hint: String::new(),
    }
}

fn provider(id: &str) -> ProviderSpec {
    ProviderSpec::new(id, "Slack", WireProtocol::OpenaiChat, &[])
}

#[test]
fn everything_declared_and_namespaced_goes_in() {
    let mut bag =
        RegistrationBag::new(&manifest(&["channels", "providers", "context", "commands"]));
    bag.add_channel(factory("slack"));
    bag.add_channel(factory("slack-dm"));
    bag.add_provider(provider("slack-models"));
    bag.add_contributor(Arc::new(Quiet));
    bag.add_command(command("slack"));

    let registration = bag.finish();
    assert!(
        registration.warnings.is_empty(),
        "{:?}",
        registration.warnings
    );
    assert_eq!(registration.channel_ids(), vec!["slack", "slack-dm"]);
    assert_eq!(registration.provider_ids(), vec!["slack-models"]);
    assert_eq!(registration.command_ids(), vec!["slack"]);
    assert_eq!(registration.contributors.len(), 1);
}

#[test]
fn a_kind_the_manifest_omits_is_dropped_with_a_sentence() {
    // Declares channels only, and tries to add one of everything else.
    let mut bag = RegistrationBag::new(&manifest(&["channels"]));
    bag.add_provider(provider("slack"));
    bag.add_contributor(Arc::new(Quiet));
    bag.add_command(command("slack"));

    let registration = bag.finish();
    assert!(registration.providers.is_empty());
    assert!(registration.contributors.is_empty());
    assert!(registration.commands.is_empty());
    assert_eq!(registration.warnings.len(), 3);
    for warning in &registration.warnings {
        assert!(warning.contains("does not declare"), "{warning}");
        assert!(warning.contains("re-approve"), "{warning}");
    }
}

#[test]
fn an_id_outside_the_extensions_own_namespace_is_dropped_with_a_sentence() {
    let mut bag = RegistrationBag::new(&manifest(&["channels", "providers", "commands"]));
    // Neither `slack` nor `slack-…`: a name another extension could claim.
    bag.add_channel(factory("general"));
    bag.add_provider(provider("slackish"));
    bag.add_command(command("post"));

    let registration = bag.finish();
    assert!(registration.channels.is_empty());
    assert!(registration.providers.is_empty());
    assert!(registration.commands.is_empty());
    assert_eq!(registration.warnings.len(), 3);
    assert!(registration.warnings[0].contains("channel \"general\""));
    assert!(registration.warnings[0].contains("\"slack\" or start with \"slack-\""));
    // `slackish` starts with the id but not with `slack-`, which is the case a
    // naive prefix check gets wrong.
    assert!(registration.warnings[1].contains("provider \"slackish\""));
    assert!(registration.warnings[2].contains("command \"post\""));
}

#[test]
fn one_bad_registration_does_not_take_the_good_ones_with_it() {
    let mut bag = RegistrationBag::new(&manifest(&["commands"]));
    bag.add_command(command("slack-one"));
    bag.add_command(command("nope"));
    bag.add_command(command("slack-two"));

    let registration = bag.finish();
    assert_eq!(registration.command_ids(), vec!["slack-one", "slack-two"]);
    assert_eq!(registration.warnings.len(), 1);
}

#[test]
fn the_declared_set_is_read_before_a_round_trip_is_spent() {
    let bag = RegistrationBag::new(&manifest(&["commands", "tools"]));
    assert!(bag.declares(ExtensionContribution::Tools));
    assert!(bag.declares(ExtensionContribution::Commands));
    assert!(!bag.declares(ExtensionContribution::Channels));
    // In the order the wire lists them, not the order the manifest wrote them.
    assert_eq!(
        bag.declared(),
        vec![
            ExtensionContribution::Tools,
            ExtensionContribution::Commands
        ]
    );
}

#[test]
fn every_kind_has_a_name_an_operator_can_read() {
    for kind in [
        ExtensionContribution::Tools,
        ExtensionContribution::Channels,
        ExtensionContribution::Providers,
        ExtensionContribution::Context,
        ExtensionContribution::Commands,
    ] {
        assert!(!kind_name(kind).is_empty());
    }
    assert_eq!(kind_name(ExtensionContribution::Context), "context");
}

#[test]
fn a_bag_says_what_it_holds_without_naming_its_contents() {
    let mut bag = RegistrationBag::new(&manifest(&["commands"]));
    bag.add_command(command("slack"));
    let rendered = format!("{bag:?}");
    assert!(rendered.contains("slack"), "{rendered}");
    assert!(rendered.contains("commands: 1"), "{rendered}");
}
