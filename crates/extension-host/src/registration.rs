//! Turning what the list methods answered into things the host can register.
//!
//! Four conversions, and each one has a rule that is not obvious:
//!
//!  - **Tools go through the MCP bridge, unchanged.** Not a copy of it, not a
//!    variant of it — [`ghostai_mcp::bridge_tool`] itself, with the prefix `ext`
//!    instead of `mcp`. That is the whole of the difference between an
//!    extension's tools and an MCP server's, and it should stay the whole of
//!    it: one bridge, two prefixes. What the bridge already decides — that a
//!    schema it cannot advertise drops *that tool* and leaves the rest working,
//!    that a remote failure is a result rather than a dead turn, that
//!    `readOnlyHint` is the one claim believed — is right here for the same
//!    reasons it is right there.
//!  - **Providers are data, converted explicitly.** The manifest's spec is not
//!    fed to serde and hoped over: its optional strings are empty rather than
//!    absent, and `defaultApiBase: ""` deserialised into `Some("")` would tell
//!    the resolver a base was supplied when the field's contract is "absent
//!    means one is required". So each is mapped by hand, and an unknown `wire`
//!    is a warning on the row rather than a manifest that will not load.
//!  - **A context contributor fetches in the static half and reads in the
//!    runtime half.** The trait says so, and out of process it is not merely
//!    advice: `runtime_section` is synchronous and is called on every
//!    iteration, so an RPC there would block a worker thread five or ten times
//!    a turn. Both halves are fetched once per turn, in the async half, and the
//!    runtime one is cached per session.
//!  - **A channel is built over the same connection.** The factory the host
//!    registers closes over the RPC client, so starting a channel is
//!    `ghostai/channels/start` and sending on it is `ghostai/channels/send`.

use std::sync::Arc;

use ghostai_mcp::{BridgeOptions, McpCallTarget, McpToolDescriptor, bridge_tool};
use ghostai_protocol::{ExtensionProviderSpec, ToolSource};
use ghostai_providers::{ProviderSpec, WireProtocol};

use crate::bag::RegistrationBag;

/// The prefix that keeps an extension's tools out of every other namespace.
///
/// `ext_<id>_<tool>`, which is what the MCP bridge's flattener produces from
/// this and the extension id. The MCP manager passes `mcp`; nothing else does.
pub const EXTENSION_TOOL_PREFIX: &str = "ext";

/// Bridges one descriptor and records it, or records why it could not be.
///
/// The issues the bridge reports are warnings on the row either way: a tool it
/// refused is one tool missing from an otherwise working extension, which is
/// worth saying and not worth failing over.
pub fn add_bridged_tool(
    bag: &mut RegistrationBag,
    extension_id: &str,
    descriptor: &McpToolDescriptor,
    target: Arc<dyn McpCallTarget>,
) {
    let options = BridgeOptions::new(EXTENSION_TOOL_PREFIX, extension_id, descriptor)
        // Without this the definition says "from the hello MCP server", which
        // is the one place the shared bridge has to be told which side it is
        // serving.
        .source(ToolSource::Extension);
    let bridged = bridge_tool(descriptor, target, options);
    for issue in &bridged.issues {
        bag.warn(format!(
            "The tool \"{}\" has a schema problem: {}",
            issue.tool, issue.message
        ));
    }
    match bridged.tool {
        Some(tool) => bag.add_tool(tool),
        None => bag.warn(format!(
            "The tool \"{}\" could not be advertised and was dropped.",
            bridged.upstream_name
        )),
    }
}

/// An empty optional string is absent, not present-and-empty.
fn optional(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

/// Which wire a spec names, or `None` when this build has no adapter for it.
fn wire_of(wire: &str) -> Option<WireProtocol> {
    [
        WireProtocol::OpenaiChat,
        WireProtocol::AnthropicMessages,
        WireProtocol::GeminiGenerate,
        WireProtocol::OpenaiResponses,
    ]
    .into_iter()
    .find(|candidate| candidate.as_str() == wire)
}

/// Converts one manifest provider spec, or says why it cannot be registered.
///
/// A wire this build lacks is the only refusal, and it is a warning rather than
/// a parse failure by construction: the manifest declares `wire` as a plain
/// string precisely so that an extension written against a newer host still
/// installs on this one, minus the provider it could not supply.
pub fn add_provider(bag: &mut RegistrationBag, spec: &ExtensionProviderSpec) {
    let Some(wire) = wire_of(&spec.wire) else {
        bag.warn(format!(
            "The provider \"{}\" names the wire \"{}\", which this build has no adapter for. \
             It was not registered.",
            spec.id, spec.wire
        ));
        return;
    };

    bag.add_provider(ProviderSpec {
        id: spec.id.clone(),
        display_name: if spec.display_name.is_empty() {
            spec.id.clone()
        } else {
            spec.display_name.clone()
        },
        wire,
        keywords: spec.keywords.clone(),
        env_key: optional(&spec.env_key),
        default_api_base: optional(&spec.default_api_base),
        is_local: spec.is_local,
        is_gateway: spec.is_gateway,
        is_o_auth: spec.is_o_auth,
        detect_by_key_prefix: optional(&spec.detect_by_key_prefix),
        detect_by_base_keyword: optional(&spec.detect_by_base_keyword),
        strip_model_prefix: spec.strip_model_prefix,
        preserve_model_prefix: spec.preserve_model_prefix,
        max_tokens_param: max_tokens_param(spec),
        default_headers: spec.default_headers.clone(),
        // Deliberately not in the manifest schema. `modelOverrides` and
        // `reasoningOffBody` are how the shipped table works around one named
        // endpoint's quirks; an extension that needs either is describing a
        // wire this build does not have, which is the warning above.
        model_overrides: Vec::new(),
        supports_prompt_caching: spec.supports_prompt_caching,
        reasoning_off_body: None,
        supports_model_listing: spec.supports_model_listing,
    });
}

fn max_tokens_param(spec: &ExtensionProviderSpec) -> ghostai_providers::MaxTokensParam {
    match spec.max_tokens_param {
        ghostai_protocol::ExtensionMaxTokensParam::MaxTokens => {
            ghostai_providers::MaxTokensParam::MaxTokens
        }
        ghostai_protocol::ExtensionMaxTokensParam::MaxCompletionTokens => {
            ghostai_providers::MaxTokensParam::MaxCompletionTokens
        }
    }
}
