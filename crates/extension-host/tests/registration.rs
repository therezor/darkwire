//! Manifest data into a provider table entry, and a descriptor into a tool.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use darkwire_core::Result;
use darkwire_extension_host::{
    EXTENSION_TOOL_PREFIX, RegistrationBag, add_bridged_tool, add_provider,
};
use darkwire_mcp::{McpCallOptions, McpCallResult, McpCallTarget, McpToolDescriptor};
use darkwire_protocol::json::Object;
use darkwire_protocol::{ExtensionManifest, ExtensionProviderSpec, ToolSource};
use darkwire_providers::WireProtocol;
use futures::future::BoxFuture;
use serde_json::json;

fn manifest(contributes: &[&str]) -> ExtensionManifest {
    serde_json::from_value(json!({
        "schema": "darkwire.extension/2",
        "id": "slack",
        "command": ["node", "index.mjs"],
        "contributes": contributes,
    }))
    .unwrap()
}

fn spec(value: serde_json::Value) -> ExtensionProviderSpec {
    serde_json::from_value(value).unwrap()
}

struct Silent;

impl McpCallTarget for Silent {
    fn call(
        &self,
        _upstream_name: &str,
        _args: Object,
        _options: McpCallOptions,
    ) -> BoxFuture<'_, Result<McpCallResult>> {
        Box::pin(async { Ok(McpCallResult::text("")) })
    }
}

fn descriptor(name: &str, schema: serde_json::Value) -> McpToolDescriptor {
    McpToolDescriptor {
        name: name.to_owned(),
        title: None,
        description: Some("Posts a message.".to_owned()),
        input_schema: schema,
        annotations: None,
    }
}

fn object_schema() -> serde_json::Value {
    json!({"type": "object", "properties": {}, "additionalProperties": false})
}

#[test]
fn a_tool_is_bridged_under_the_extension_prefix() {
    assert_eq!(EXTENSION_TOOL_PREFIX, "ext");
    let mut bag = RegistrationBag::new(&manifest(&["tools"]));
    add_bridged_tool(
        &mut bag,
        "slack",
        &descriptor("post message", object_schema()),
        Arc::new(Silent),
    );

    let registration = bag.finish();
    assert_eq!(registration.tools.len(), 1);
    // The name is rewritten rather than refused: tool names have their own
    // character class, and an extension writing `post message` is asking a
    // reasonable thing the flattener knows how to grant.
    assert_eq!(registration.tool_names(), vec!["ext_slack_post-message"]);
    // And the definition says whose it is on the extension side of the bridge.
    assert_eq!(
        registration.tools[0].definition().source,
        ToolSource::Extension
    );
}

#[test]
fn a_schema_the_host_cannot_advertise_drops_one_tool_and_keeps_the_rest() {
    let mut bag = RegistrationBag::new(&manifest(&["tools"]));
    add_bridged_tool(
        &mut bag,
        "slack",
        &descriptor("broken", json!("not a schema at all")),
        Arc::new(Silent),
    );
    add_bridged_tool(
        &mut bag,
        "slack",
        &descriptor("fine", object_schema()),
        Arc::new(Silent),
    );

    let registration = bag.finish();
    assert_eq!(registration.tool_names(), vec!["ext_slack_fine"]);
    assert!(!registration.warnings.is_empty());
}

#[test]
fn a_provider_becomes_a_table_entry_with_its_empty_strings_absent() {
    let mut bag = RegistrationBag::new(&manifest(&["providers"]));
    add_provider(
        &mut bag,
        &spec(json!({
            "id": "slack-gpt",
            "displayName": "Slack GPT",
            "wire": "openai-chat",
            "keywords": ["slack"],
            "defaultApiBase": "https://example.invalid/v1",
            "isGateway": true,
            "maxTokensParam": "max_completion_tokens",
            "supportsModelListing": true,
        })),
    );

    let registration = bag.finish();
    assert!(registration.warnings.is_empty());
    let provider = &registration.providers[0];
    assert_eq!(provider.id, "slack-gpt");
    assert_eq!(provider.wire, WireProtocol::OpenaiChat);
    assert_eq!(
        provider.default_api_base.as_deref(),
        Some("https://example.invalid/v1")
    );
    assert!(provider.is_gateway);
    assert!(provider.supports_model_listing);
    // The fields the manifest left empty are *absent*, not empty strings: the
    // resolver reads "absent" as "the operator must supply one".
    assert_eq!(provider.env_key, None);
    assert_eq!(provider.detect_by_key_prefix, None);
    assert_eq!(provider.detect_by_base_keyword, None);
    assert_eq!(
        provider.max_tokens_param,
        darkwire_providers::MaxTokensParam::MaxCompletionTokens
    );
    // Not expressible from a manifest, deliberately.
    assert!(provider.model_overrides.is_empty());
    assert_eq!(provider.reasoning_off_body, None);
}

#[test]
fn a_provider_with_no_display_name_falls_back_to_its_id() {
    let mut bag = RegistrationBag::new(&manifest(&["providers"]));
    add_provider(&mut bag, &spec(json!({"id": "slack"})));
    let registration = bag.finish();
    assert_eq!(registration.providers[0].display_name, "slack");
    // The manifest's own default, so an omitted `wire` is the common case.
    assert_eq!(registration.providers[0].wire, WireProtocol::OpenaiChat);
}

#[test]
fn a_wire_this_build_lacks_is_a_row_warning_and_not_a_refusal() {
    let mut bag = RegistrationBag::new(&manifest(&["providers"]));
    add_provider(
        &mut bag,
        &spec(json!({"id": "slack", "wire": "cohere-generate"})),
    );
    add_provider(
        &mut bag,
        &spec(json!({"id": "slack-two", "wire": "gemini-generate"})),
    );

    let registration = bag.finish();
    // The one this build knows is registered; the other is a sentence.
    assert_eq!(registration.provider_ids(), vec!["slack-two"]);
    assert_eq!(registration.warnings.len(), 1);
    assert!(
        registration.warnings[0].contains("cohere-generate"),
        "{:?}",
        registration.warnings
    );
    assert!(registration.warnings[0].contains("no adapter"));
}

#[test]
fn a_provider_the_manifest_did_not_declare_is_still_dropped() {
    // `providers` is manifest data, so it never goes through a list method —
    // and the declaration still has to be honest about it.
    let mut bag = RegistrationBag::new(&manifest(&["tools"]));
    add_provider(&mut bag, &spec(json!({"id": "slack"})));
    let registration = bag.finish();
    assert!(registration.providers.is_empty());
    assert!(registration.warnings[0].contains("does not declare"));
}
