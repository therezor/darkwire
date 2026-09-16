//! The system prompt, in two halves.
//!
//! A provider's prompt cache keys on an exact prefix: the longest run of
//! leading tokens identical to the previous request is free, and the first
//! differing token ends the discount for everything after it. A single prompt
//! carrying the current time therefore costs full price on every iteration of
//! every turn — and a tool-using turn is five or ten requests over the same
//! history.
//!
//! So the prompt is assembled from a **static half** and a **runtime half**:
//!
//!  - The static half — the agent's identity template plus whatever the
//!    contributors add — is byte-identical for the life of a session. It is the
//!    cached prefix, and everything that goes in it must be stable: a
//!    timestamp, an iteration counter or a per-turn nonce placed here
//!    invalidates the cache for the whole session, which is the exact cost this
//!    split exists to avoid.
//!  - The runtime half — live state, the turn's delimiter, a correction — is
//!    rewritten before every request. It must be the last thing in the
//!    *request*, not merely the last thing in the system message.
//!
//! **That distinction is the whole point.** Appended to the system message —
//! `messages[0]`, the *front* of the request — the runtime half sits ahead of
//! the entire conversation, so a changed iteration counter ends the discount
//! for all of it on every request: a ten-iteration turn over a long history
//! pays for that history ten times. So the halves are composed into different
//! messages, and the loop sends the runtime half as a trailing turn after the
//! history:
//!
//! ```text
//! system( static_prompt )     ← cached, session-stable
//! tools                       ← cached, stable per turn
//! ...history                  ← cached, append-only
//! user( <system-reminder> )   ← the only part re-read at full price
//! ```
//!
//! A trailing *user* message rather than a second system one: two system
//! messages is a shape some providers reject and others quietly reorder, and
//! the ordering is what the cache depends on. The loop wraps it so the model
//! reads it as operator metadata rather than as something the user typed.
//!
//! The tool-output policy follows the same reasoning. It is the largest block
//! in the prompt that never changes, and the only thing tying it to the runtime
//! half was the per-turn delimiter it named — so the delimiter is one line of
//! live state and the prose is cached.
//!
//! **The identity text is not in this file.** It is a template in
//! `ghostai-protocol`, because an agent owns its whole system prompt and the
//! browser edits it — so the wording and the substitution rules have to be one
//! definition. This module owns the *facts* it is rendered with, which are the
//! ones only the host knows: the platform, the architecture and the version.
//! Protocol owns the shape; agent owns the values.
//!
//! ## Who decides what
//!
//! Three jobs, in three places, and fusing any two of them is what this note
//! guards against.
//!
//!  1. **What a section says** — the operator, through the config. The
//!     three-state encoding of their wording (empty, a single space, anything
//!     else) is the config's; it is decoded here at [`template_or`] and
//!     `render_tool_policy`, and nothing above this module writes values into
//!     it.
//!  2. **Whether a section applies to this turn** — the caller, and only the
//!     caller. The loop knows whether the model is being sent tools; this
//!     module never asks and is never told. It is expressed by handing over
//!     [`PromptTools`] or not, so the answer arrives as the presence of an
//!     input rather than as a flag to branch on.
//!  3. **What the prompt looks like** — this module. Render each section from
//!     its inputs, place it in the half that fits, join.
//!
//! The rule that falls out: **a section that does not apply has no input, and a
//! section with no input renders nothing.** No builder below takes a boolean
//! saying a feature is off. The tool-output policy proves it — which half emits
//! it is the operator's template's decision, so *two* builders place it, and a
//! gate written as a condition inside a builder would have to be written twice,
//! correctly, forever.

use std::sync::LazyLock;

use chrono::{DateTime, TimeZone as _, Utc};
use chrono_tz::Tz;
use ghostai_protocol::json::js_trim;
use ghostai_protocol::{
    DEFAULT_LIVE_STATE_TEMPLATE, DEFAULT_PLATFORM_CONTAINER_TEMPLATE,
    DEFAULT_PLATFORM_HOST_TEMPLATE, DEFAULT_SYSTEM_PROMPT_TEMPLATE, DEFAULT_WRAP_UP_TEMPLATE,
    PromptMode, SECTION_SEPARATOR, render_prompt_template, render_wrap_up, tool_policy_uses_nonce,
};
use ghostai_providers::BoxFuture;
use ghostai_security::{tool_output_policy, tool_output_tag};
use indexmap::IndexMap;
use regex::{Captures, Regex};

/// What a contributor is told about the session. Stable for its lifetime.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StaticPromptContext {
    /// Absolute, canonical. Every tool path resolves inside it.
    pub workspace_root: String,
    /// Which workspace the session is bound to.
    ///
    /// Beside `workspace_root` rather than derived from it, because a
    /// contributor that wants to scope memory or skills per workspace needs the
    /// id, not a path.
    pub workspace_id: String,
    /// The conversation.
    pub session_key: String,
    /// The agent the *session* is bound to, which a loop's own agent may
    /// differ from.
    pub agent_id: Option<String>,
    /// The channel the turn arrived on — `cli`, `web`, `telegram`, an
    /// extension id.
    pub channel: String,
}

/// The same, plus what changes between requests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimePromptContext {
    /// Everything stable for the session.
    pub static_context: StaticPromptContext,
    /// 1-based, and reset per turn.
    pub iteration: u64,
    /// The turn's cap.
    pub max_iterations: u64,
    /// Wall-clock epoch milliseconds, from the injected clock.
    pub now_ms: i64,
}

/// A source of prompt content the loop knows nothing about.
///
/// This is the seam memory and skills arrive through. The loop's job is to
/// compose and cache; deciding that a memory index belongs in the prompt, and
/// at what budget, is a decision that would otherwise force this crate to
/// depend on every one of them and every consumer of the loop to construct one.
///
/// The two halves carry different obligations, and they are not
/// interchangeable:
///
///  - [`ContextContributor::static_section`] is called once per turn and may do
///    I/O. Its result must be stable across the session — a section that
///    changes wherever it likes hands back the cache benefit the split was
///    built for.
///  - [`ContextContributor::runtime_section`] is called on every iteration and
///    is synchronous. Anything expensive there is paid five or ten times per
///    turn, so a contributor that needs I/O should do it in the static half and
///    read the result here.
pub trait ContextContributor: Send + Sync {
    /// What to call it in a log line.
    fn name(&self) -> &str;

    /// The once-per-turn half. `None` places no section.
    fn static_section<'a>(
        &'a self,
        context: &'a StaticPromptContext,
    ) -> BoxFuture<'a, Option<String>> {
        let _ = context;
        Box::pin(std::future::ready(None))
    }

    /// The per-iteration half. `None` places no section.
    fn runtime_section(&self, context: &RuntimePromptContext) -> Option<String> {
        let _ = context;
        None
    }
}

/// Who the turn is being run by.
///
/// Every field comes from the resolved agent and is stable for the life of a
/// session, which is what lets them sit in the cached half of the prompt.
///
/// The tool-shaped templates are deliberately **not** here — they are
/// [`PromptTools`], a separate argument. An agent always has an identity; it
/// does not always have tools, and the sections that describe tools have to be
/// absent, not merely empty, on a turn that has none. Splitting them is what
/// lets a builder answer "is this section placed?" from its own inputs instead
/// of from a flag someone remembered to thread through.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptAgent {
    /// Empty falls back to `GhostAI`. Fills `{{name}}`.
    pub label: String,
    /// This agent's live-state template. Empty means the built-in.
    pub live_prompt: Option<String>,
    /// This agent's wrap-up sentence. Empty means the built-in.
    pub wrap_up_prompt: Option<String>,
    /// Whether `system_prompt` is the static half or the entire system message.
    ///
    /// Absent is `Template`, so a caller that predates raw mode keeps the
    /// assembly it had.
    pub prompt_mode: Option<PromptMode>,
    /// This agent's whole static prompt, as a template.
    ///
    /// Empty means the built-in, which is what keeps an install that never
    /// customised one receiving improvements to it. It is not appended to
    /// anything: whatever is here *is* the identity half of the prompt.
    pub system_prompt: String,
}

/// Everything the prompt says about tools — or, when absent, that it says none
/// of it.
///
/// **Absence is the meaning.** A turn whose model is sent no tools passes no
/// `tools` at all, and the three sections that describe tools then have no
/// inputs to render from: the tool-output policy and the command policy. Neither needs to
/// be told why. That is the whole reason this is a group rather than three
/// optional fields beside the agent, and it is why there is no boolean anywhere
/// below — a builder that branched on "are tools on" would be a builder that
/// has to be told twice, once per half of the prompt the policy can land in.
///
/// Each field is the *operator's wording* for its section and keeps the
/// config's three states: absent or empty inherits the built-in, a single space
/// removes the section, anything else replaces it. Those are decisions a person
/// made about a section that exists. Whether it exists at all is this object.
///
/// `confined` is the exception and the one boolean here. It is not wording: it
/// says which built-in the command policy inherits *from*, and it has to be a
/// per-turn input because a subagent runs where its caller's reference says it
/// does. Deciding it when the loop was built, once per agent, told an
/// inheriting subagent its commands run on this machine while they ran in a
/// container.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptTools {
    /// Wording for the tool-output policy — what the delimiters around a result
    /// mean.
    pub policy_prompt: Option<String>,
    /// Wording for the command policy — where `exec` lands, and what is
    /// available there.
    pub platform_prompt: Option<String>,
    /// Whether this turn's commands run away from this machine's filesystem,
    /// which decides which built-in command policy an empty `platform_prompt`
    /// inherits.
    pub confined: bool,
}

/// Which operating system a command would land on.
///
/// A closed set plus an escape hatch, because the prompt only draws a
/// consequence from three of them and every other host still has to be named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Platform {
    /// `macos`.
    MacOs,
    /// `windows`, the one that gets its own shell advice.
    Windows,
    /// `linux`.
    Linux,
    /// Anything else, named as the target reports it.
    Other(String),
}

impl Platform {
    /// This process's platform.
    pub fn host() -> Platform {
        Platform::named(std::env::consts::OS)
    }

    /// The platform a target triple's OS string names.
    pub fn named(os: &str) -> Platform {
        match os {
            "macos" => Platform::MacOs,
            "windows" => Platform::Windows,
            "linux" => Platform::Linux,
            other => Platform::Other(other.to_owned()),
        }
    }

    /// What `{{platform}}` renders as.
    pub fn as_str(&self) -> &str {
        match self {
            Platform::MacOs => "macos",
            Platform::Windows => "windows",
            Platform::Linux => "linux",
            Platform::Other(name) => name,
        }
    }

    /// What a person calls it, for the runtime line.
    pub fn label(&self) -> &str {
        match self {
            Platform::MacOs => "macOS",
            Platform::Windows => "Windows",
            Platform::Linux => "Linux",
            Platform::Other(name) => name,
        }
    }
}

/// The host as both builders describe it.
///
/// Defaulted once rather than in each builder: a raw agent and a template agent
/// on the same machine must be told the same thing about it, and two copies of
/// a default is how that stops being true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// Where a command would run.
    pub platform: Platform,
    /// The `<os> <arch>, GhostAI <version>` line, or an override.
    pub runtime_label: String,
}

impl Default for Host {
    fn default() -> Host {
        let platform = Platform::host();
        let runtime_label = format!(
            "{} {}, GhostAI {}",
            platform.label(),
            std::env::consts::ARCH,
            env!("CARGO_PKG_VERSION")
        );
        Host {
            platform,
            runtime_label,
        }
    }
}

/// What [`build_static_prompt`] is given.
pub struct BuildStaticPrompt<'a> {
    /// The session.
    pub context: &'a StaticPromptContext,
    /// Absent is the unnamed default agent: the built-in template, rendered as
    /// `GhostAI`.
    pub agent: Option<&'a PromptAgent>,
    /// Sections the loop knows nothing about.
    pub contributors: &'a [&'a dyn ContextContributor],
    /// Absent means this model is sent no tools, so no tool-shaped section is
    /// placed.
    pub tools: Option<&'a PromptTools>,
    /// Injected so the prompt is assertable without depending on the test host.
    pub host: Host,
}

impl<'a> BuildStaticPrompt<'a> {
    /// The unnamed default agent, no contributors, and a model with no tools.
    ///
    /// Every other field is a deliberate addition, which is what keeps "this
    /// turn has no tools" expressible as leaving one out rather than as a flag.
    pub fn new(context: &'a StaticPromptContext) -> BuildStaticPrompt<'a> {
        BuildStaticPrompt {
            context,
            agent: None,
            contributors: &[],
            tools: None,
            host: Host::default(),
        }
    }
}

/// What [`build_runtime_block`] is given.
pub struct BuildRuntimeBlock<'a> {
    /// The iteration.
    pub context: &'a RuntimePromptContext,
    /// This agent's live-state template. Empty means the built-in; a single
    /// space removes the section.
    pub live_prompt: Option<&'a str>,
    /// This agent's wrap-up sentence, on the same contract.
    pub wrap_up_prompt: Option<&'a str>,
    /// Absent means this model is sent no tools, so no tool-shaped section is
    /// placed. Only `policy_prompt` is read here — and only when it names the
    /// delimiter, which is what moves it out of the cached half and into this
    /// one.
    ///
    /// The same object the static half receives, rather than the one field this
    /// half happens to use, because which half emits the policy is the
    /// operator's decision and not this signature's.
    pub tools: Option<&'a PromptTools>,
    /// This turn's tool-output nonce.
    pub nonce: &'a str,
    /// Sections the loop knows nothing about.
    pub contributors: &'a [&'a dyn ContextContributor],
    /// IANA zone name. Absent reads the host's.
    pub time_zone: Option<&'a str>,
    /// A correction for one iteration, about what the previous one did wrong.
    ///
    /// Here rather than as a message in the conversation because the runtime
    /// half is rebuilt every iteration regardless, so it costs no cached prefix
    /// and leaves nothing behind in history — a correction appended as a `user`
    /// message would read in the transcript as something the operator said.
    pub correction: Option<&'a str>,
}

impl<'a> BuildRuntimeBlock<'a> {
    /// The built-in wording, no contributors, the host zone and no tools.
    pub fn new(context: &'a RuntimePromptContext, nonce: &'a str) -> BuildRuntimeBlock<'a> {
        BuildRuntimeBlock {
            context,
            live_prompt: None,
            wrap_up_prompt: None,
            tools: None,
            nonce,
            contributors: &[],
            time_zone: None,
            correction: None,
        }
    }
}

/// What [`build_raw_prompt`] is given.
pub struct BuildRawPrompt<'a> {
    /// The iteration.
    pub context: &'a RuntimePromptContext,
    /// The agent whose one template this is.
    pub agent: Option<&'a PromptAgent>,
    /// Absent means this model is sent no tools, so `{{environment}}`,
    /// `{{toolPolicy}}` and `{{platformPolicy}}` render to nothing.
    ///
    /// Rendering to nothing rather than being dropped is the only answer raw
    /// mode can give: the operator placed those placeholders, so the layout
    /// around them is theirs and this is not free to remove a line they wrote.
    pub tools: Option<&'a PromptTools>,
    /// Injected so the prompt is assertable without depending on the test host.
    pub host: Host,
    /// This turn's tool-output nonce.
    pub nonce: &'a str,
    /// The static contributor sections, already collected.
    ///
    /// Passed in rather than gathered here because this runs on every iteration
    /// and a static section may do I/O. The caller holds the once-per-turn
    /// result; see [`contributor_sections`].
    pub static_sections: &'a [String],
    /// Sections the loop knows nothing about.
    pub contributors: &'a [&'a dyn ContextContributor],
    /// IANA zone name. Absent reads the host's.
    pub time_zone: Option<&'a str>,
    /// A correction for one iteration. See [`BuildRuntimeBlock::correction`].
    pub correction: Option<&'a str>,
}

impl<'a> BuildRawPrompt<'a> {
    /// The built-in template, no contributors, the host zone and no tools.
    pub fn new(context: &'a RuntimePromptContext, nonce: &'a str) -> BuildRawPrompt<'a> {
        BuildRawPrompt {
            context,
            agent: None,
            tools: None,
            host: Host::default(),
            nonce,
            static_sections: &[],
            contributors: &[],
            time_zone: None,
            correction: None,
        }
    }
}

/// An operator's template, or the built-in.
///
/// Whitespace-only is *not* empty here, and that asymmetry is deliberate: empty
/// means "I have not chosen", which must keep inheriting improvements to the
/// default, while a single space is the only way to say "I want this section
/// gone". An agent's `system_prompt` treats whitespace as empty because an
/// identity-less agent is never what anyone meant; an install with no
/// live-state section is coherent.
///
/// Public for `memory_contributor`, which owns a template this file does not
/// place. A second spelling of the rule is how the seven come to disagree about
/// what a space means.
pub fn template_or<'a>(stored: Option<&'a str>, fallback: &'a str) -> &'a str {
    match stored {
        None | Some("") => fallback,
        Some(text) => text,
    }
}

fn values(pairs: impl IntoIterator<Item = (&'static str, String)>) -> IndexMap<String, String> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

/// Where commands run, and what that place is like.
///
/// Its own section in template mode, and `{{platformPolicy}}` in raw mode. It
/// is generated rather than written into the identity because it is the one
/// part of the static half that depends on *placement*. The same agent text has
/// to be true whether `exec` lands on the host or in a container, and those two
/// are opposite on every point that matters: whether the workspace confines the
/// command, whether a shell is available, and which OS's tools exist.
///
/// Getting that wrong is not cosmetic. The host wording sent to a containered
/// agent tells it its commands run on macOS (they run in Alpine), that they are
/// *not* confined to the workspace (only the workspace is mounted, so they
/// are), and on a Windows host that GNU tools might be missing (the container
/// has them). A model resolving a contradiction between its prompt and its
/// tools tends to resolve it by refusing.
///
/// The **file tools are placement-independent** and that sentence is the one
/// most worth its tokens: they always act on the workspace on this machine,
/// through the jail, whatever `exec` does. Without it a model has no way to
/// know that the file it wrote and the file a command sees are the same file
/// under two names.
fn command_policy(host: &Host, workspace_id: &str, tools: Option<&PromptTools>) -> String {
    // No tools, no commands, nothing to say about where they land. First,
    // because every line below describes running one — including the sentence
    // about the file tools, which are tools too.
    let Some(tools) = tools else {
        return String::new();
    };

    // Which built-in an empty override inherits is a property of *this turn*,
    // not of the agent: an agent that names no environment of its own runs
    // where its caller does, so the same loop can be confined on one turn and
    // not on the next.
    let stored = template_or(
        tools.platform_prompt.as_deref(),
        if tools.confined {
            DEFAULT_PLATFORM_CONTAINER_TEMPLATE
        } else {
            DEFAULT_PLATFORM_HOST_TEMPLATE
        },
    );
    if js_trim(stored).is_empty() {
        return String::new();
    }

    let shell = if host.platform == Platform::Windows {
        "\n\n- Do not assume GNU tools such as `grep`, `sed` or `awk` are installed.
- Prefer the file tools over shelling out; prefer Windows-native commands when you must.
- If command output comes back garbled, re-run it with UTF-8 output enabled."
            .to_owned()
    } else {
        "\n\n- Standard shell tools and UTF-8 are available.
- Prefer the file tools where they are simpler or more reliable than a command."
            .to_owned()
    };

    // Bare text, no leading break. It is a section, and the separator between
    // sections is the caller's to write — which is the whole reason it stopped
    // being a `{{platformPolicy}}` placeholder in the identity template. A
    // placeholder renders to a string and an empty one leaves the blank lines
    // the template wrote around it; a section that does not apply is simply not
    // in the list.
    let rendered = render_prompt_template(
        stored,
        &values([
            ("runtime", host.runtime_label.clone()),
            ("platform", host.platform.as_str().to_owned()),
            ("workspaceId", workspace_id.to_owned()),
            ("shellPolicy", shell),
        ]),
    );
    js_trim(&rendered).to_owned()
}

/// Every contributor's static section, joined and trimmed.
///
/// Separate from [`build_static_prompt`] because raw mode needs the same value
/// in a different place — as `{{contributors}}` rather than as trailing
/// sections — and both modes have to keep the one obligation that matters: a
/// static section may do I/O and is therefore called **once per turn**, never
/// per iteration.
pub async fn contributor_sections(
    contributors: &[&dyn ContextContributor],
    context: &StaticPromptContext,
) -> Vec<String> {
    let mut sections = Vec::new();
    for contributor in contributors {
        if let Some(section) = contributor.static_section(context).await {
            let trimmed = js_trim(&section);
            if !trimmed.is_empty() {
                sections.push(trimmed.to_owned());
            }
        }
    }
    sections
}

/// The identity section, rendered from whatever template this agent carries.
///
/// The text itself lives in `ghostai-protocol`, not here, and an agent that
/// stores its own replaces it wholesale — heading, workspace rules, platform
/// note, guidelines and all.
///
/// **The tempting alternative is to splice an operator's paragraph into a fixed
/// block**, on the grounds that the workspace semantics and the guidelines are
/// "not an operator's to replace by writing a persona". That objection does not
/// survive contact with what those sentences actually are: prose telling the
/// model what is true. The jail and the exec guard live in `ghostai-security`, are
/// enforced on every call, and have never read a word of this. An operator who
/// deletes the workspace paragraph gets an agent that is less well informed
/// about a sandbox that is exactly as tight as it was before — and in exchange,
/// the prompt an install actually runs on is one they can read and edit rather
/// than one compiled into the binary.
///
/// The tool-output policy is a separate section for the same reason, and is
/// separately overridable — it explains a mechanism rather than being one, so
/// replacing the identity does not silently delete it.
fn identity(context: &StaticPromptContext, host: &Host, agent: Option<&PromptAgent>) -> String {
    let label = agent.map_or("", |agent| agent.label.as_str());
    let stored = agent.map_or("", |agent| agent.system_prompt.as_str());

    // Whitespace-only is empty. A template of three newlines is not a decision
    // an operator made, and rendering it would give the agent no identity.
    let template = if js_trim(stored).is_empty() {
        DEFAULT_SYSTEM_PROMPT_TEMPLATE
    } else {
        stored
    };

    render_prompt_template(
        template,
        &values([
            (
                "name",
                if label.is_empty() {
                    "GhostAI".to_owned()
                } else {
                    label.to_owned()
                },
            ),
            ("workspaceId", context.workspace_id.clone()),
            // Still supplied, and no longer used by the default template:
            // handing a model the absolute root is worse than withholding it. A
            // custom prompt that asks for it still gets it.
            ("workspaceRoot", context.workspace_root.clone()),
            ("runtime", host.runtime_label.clone()),
        ]),
    )
}

/// The cache-stable half. Built once per turn; identical across the session.
///
/// Contributor sections are appended in the order given, after the built-in
/// ones, so the prefix a provider caches grows at the end rather than shifting
/// when a contributor is added or removed mid-session.
pub async fn build_static_prompt(options: BuildStaticPrompt<'_>) -> String {
    // One section, not two. The operator's text *is* the identity rather than a
    // separate `## Instructions` block below a fixed one, so there is nothing
    // to append it to. A template that renders to nothing contributes no
    // section rather than an empty one.
    let rendered = identity(options.context, &options.host, options.agent);
    let rendered = js_trim(&rendered);

    let mut sections: Vec<String> = if rendered.is_empty() {
        Vec::new()
    } else {
        vec![rendered.to_owned()]
    };

    let commands = command_policy(&options.host, &options.context.workspace_id, options.tools);
    if !commands.is_empty() {
        sections.push(commands);
    }

    // The tool-output policy, when it names no delimiter — which the default
    // does not. It is the largest block in the prompt that never changes, so
    // leaving it in the per-iteration half meant re-sending two hundred tokens
    // on every request of every turn to say something the model had already
    // been told. `build_runtime_block` places it instead when a custom template
    // asks for the tag, and the two conditions are exact complements, so it
    // appears once.
    let policy = static_tool_policy(options.tools);
    if !policy.is_empty() {
        sections.push(policy);
    }

    sections.extend(contributor_sections(options.contributors, options.context).await);

    sections.join(SECTION_SEPARATOR)
}

/// How few iterations must remain before the model is told about it.
///
/// Printed on every iteration the counter is, at iteration 1, a fact with no
/// consequence — and this block is in the *uncached* half, so it would be
/// re-sent on every request of every turn to say nothing. Near the cap it is
/// the opposite: a model that knows it has two calls left can report what it
/// has instead of being cut off mid-search.
const ITERATION_WARNING_AT: i64 = 3;

/// The tool-output policy, or nothing when the operator deleted it.
///
/// The same "empty inherits, whitespace deletes" rule as the live-state block,
/// kept in one function because raw mode fills `{{toolPolicy}}` from it too.
/// What a deletion costs is the *explanation*: every result is still wrapped
/// and a forged delimiter is still escaped, so the envelopes remain and the
/// model is simply never told what they mean.
fn render_tool_policy(template: Option<&str>, nonce: Option<&str>) -> String {
    let stored = template.unwrap_or("");
    if !stored.is_empty() && js_trim(stored).is_empty() {
        return String::new();
    }
    // The only failure is a nonce too short to make a tag from, and the caller
    // that passes one always passes the turn's. An empty policy is a better
    // answer than a turn that cannot build its prompt.
    tool_output_policy(nonce, Some(stored)).unwrap_or_default()
}

/// The policy for raw mode's `{{toolPolicy}}`, delimiter included.
///
/// Template mode splits the prose from the tag it refers to so the prose can be
/// cached; raw mode has one blob and nothing to gain from that, so the two are
/// rejoined here. A policy that names the tag itself already says it, and gets
/// no second line.
fn raw_tool_policy(template: Option<&str>, nonce: &str) -> String {
    let policy = render_tool_policy(template, Some(nonce));
    if policy.is_empty() || tool_policy_uses_nonce(template) {
        return policy;
    }
    match tool_output_tag(nonce) {
        Ok(tag) => format!("{policy}\n\nTool output delimiter: {tag}"),
        Err(_) => policy,
    }
}

/// Which half of the prompt this agent's tool-output policy belongs in.
///
/// The default policy names no delimiter, so it is identical for the life of a
/// session and goes in the cached prefix. An operator who put `{{tag}}` back
/// gets the old placement — correct output, and the caching cost the editor
/// warns about — rather than a broken prompt or a config migration.
fn tool_policy_is_static(template: Option<&str>) -> bool {
    let stored = template.unwrap_or("");
    if !stored.is_empty() && js_trim(stored).is_empty() {
        return false;
    }
    !tool_policy_uses_nonce(Some(stored))
}

/// The policy as the cached half should carry it — nothing, when it belongs to
/// the other half or when there are no tools to have output.
///
/// A pair with `runtime_tool_policy` below, and they are exact complements on
/// the `tool_policy_is_static` test, so between them the section appears once or
/// not at all. Written as two named functions rather than as a condition at
/// each call site because "not placed here" has three causes — no tools, a
/// deleted template, the other half owns it — and a reader should have to hold
/// one name rather than three.
fn static_tool_policy(tools: Option<&PromptTools>) -> String {
    let Some(tools) = tools else {
        return String::new();
    };
    if !tool_policy_is_static(tools.policy_prompt.as_deref()) {
        return String::new();
    }
    render_tool_policy(tools.policy_prompt.as_deref(), None)
}

/// The policy as the per-iteration half should carry it. See
/// [`static_tool_policy`].
fn runtime_tool_policy(tools: Option<&PromptTools>, nonce: &str) -> String {
    let Some(tools) = tools else {
        return String::new();
    };
    if tool_policy_is_static(tools.policy_prompt.as_deref()) {
        return String::new();
    }
    render_tool_policy(tools.policy_prompt.as_deref(), Some(nonce))
}

/// The instant, in the two forms a model actually needs.
///
/// The instant has to be unambiguous, which is what the ISO stamp is for; the
/// local reading is what a person's question means by "this afternoon", and the
/// weekday is what "this weekend" needs. Milliseconds are dropped — no question
/// has ever turned on them.
///
/// An unknown zone name falls back to UTC rather than failing. A prompt that
/// cannot be built fails every turn on that agent, and UTC is a worse answer
/// than the operator's zone and a far better one than no time at all.
fn live_time(now_ms: i64, time_zone: &str) -> String {
    let when: DateTime<Utc> = Utc
        .timestamp_millis_opt(now_ms)
        .single()
        .unwrap_or_default();
    let iso = when.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let local = match time_zone.parse::<Tz>() {
        Ok(zone) => when
            .with_timezone(&zone)
            .format("%A, %-d %B %Y at %H:%M")
            .to_string(),
        Err(_) => when.format("%Y-%m-%d %H:%M").to_string(),
    };
    // No `Current time:` label — that belongs to the template, which an
    // operator may reword. This returns the value the template names.
    format!("{local} ({time_zone}) — {iso}")
}

/// The zone the prompt's clock is printed in when nobody named one.
fn host_time_zone() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_owned())
}

/// The live-state values, which both halves of the prompt describe identically.
///
/// [`build_runtime_block`] renders them into the live-state template;
/// [`build_raw_prompt`] spreads them into the operator's one blob. Same values
/// either way — the iteration counter counted the same way, the wrap-up gated
/// at the same point, the clock read in the same zone — because a raw agent
/// reading a different "iterations left" than a template agent would be a
/// difference nobody chose.
fn live_values(
    context: &RuntimePromptContext,
    time_zone: Option<&str>,
    wrap_up_prompt: Option<&str>,
    nonce: &str,
) -> IndexMap<String, String> {
    let zone = time_zone.map_or_else(host_time_zone, str::to_owned);
    // Counted inclusively — on the last legal iteration one is left, not none —
    // because "1 left" is what a person and a model both read as "this is it".
    let left = i64::try_from(context.max_iterations).unwrap_or(i64::MAX)
        - i64::try_from(context.iteration).unwrap_or(i64::MAX)
        + 1;
    let wrap_up = if context.max_iterations > 0 && left <= ITERATION_WARNING_AT {
        render_wrap_up(template_or(wrap_up_prompt, DEFAULT_WRAP_UP_TEMPLATE), left)
    } else {
        String::new()
    };

    values([
        ("time", live_time(context.now_ms, &zone)),
        // Only when it is about to matter. See `ITERATION_WARNING_AT`.
        ("wrapUp", wrap_up),
        ("iteration", context.iteration.to_string()),
        ("maxIterations", context.max_iterations.to_string()),
        ("iterationsLeft", left.max(0).to_string()),
        ("channel", context.static_context.channel.clone()),
        ("sessionKey", context.static_context.session_key.clone()),
        // The one part of the tool-output policy that changes. The prose that
        // explains what it means is in the cached half; this names the value it
        // refers to.
        ("tag", tool_output_tag(nonce).unwrap_or_default()),
    ])
}

/// The contributors' per-iteration sections, trimmed and emptied out.
fn runtime_sections_of(
    contributors: &[&dyn ContextContributor],
    context: &RuntimePromptContext,
) -> Vec<String> {
    contributors
        .iter()
        .filter_map(|contributor| contributor.runtime_section(context))
        .filter_map(|section| {
            let trimmed = js_trim(&section);
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        })
        .collect()
}

/// The per-iteration half.
///
/// The tool-output policy lives here only when the operator's template names
/// the turn's nonce. The nonce is regenerated every turn; in the static half it
/// would invalidate the session's cached prefix on every single turn, which is
/// precisely the cost this file is organised to avoid.
///
/// **What is *not* here is the point.** The channel, the session key and an
/// iteration counter on every request are the tempting additions, and nothing
/// in the prompt would say what any of them meant:
///
///  - **The session key** is a UUID. Twenty tokens of random string per
///    request, for an identifier the model cannot use and might echo at the
///    user.
///  - **The channel** — `web`, `cli` — described a difference the prompt never
///    drew a consequence from. If one is wanted later it belongs in the static
///    half as advice, not as a bare label.
///  - **The iteration counter** is only actionable near the cap, so it is now
///    printed only there.
///
/// [`RuntimePromptContext`] still carries all three: contributors receive it,
/// and a memory or skills section may well want to scope by session or channel.
/// This is about what reaches the *model*.
pub fn build_runtime_block(options: &BuildRuntimeBlock<'_>) -> String {
    let live = live_values(
        options.context,
        options.time_zone,
        options.wrap_up_prompt,
        options.nonce,
    );

    let rendered = render_prompt_template(
        template_or(options.live_prompt, DEFAULT_LIVE_STATE_TEMPLATE),
        &live,
    );
    let rendered = js_trim(&rendered);
    let mut sections: Vec<String> = if rendered.is_empty() {
        Vec::new()
    } else {
        vec![rendered.to_owned()]
    };

    sections.extend(runtime_sections_of(options.contributors, options.context));

    // Only a policy that names the delimiter — the complement of the condition
    // in `build_static_prompt`, so exactly one of the two places emits it. A
    // default policy is prose about a mechanism and belongs in the cached half;
    // one that spells the tag out has to be rebuilt with the turn.
    let policy = runtime_tool_policy(options.tools, options.nonce);
    if !policy.is_empty() {
        sections.push(policy);
    }

    // Last, so it is the final thing read before the model answers. A
    // correction buried above a few hundred tokens of policy is a correction
    // competing with them for attention, and it only exists for one iteration.
    if let Some(correction) = options.correction {
        let trimmed = js_trim(correction);
        if !trimmed.is_empty() {
            sections.push(trimmed.to_owned());
        }
    }

    sections.join("\n\n")
}

/// Marks the trailing turn as operator metadata rather than something a person
/// typed.
const REMINDER_TAG: &str = "system-reminder";

/// Matches an opening or closing reminder delimiter, either case.
static REMINDER_DELIMITER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<(/?)(system-reminder)").unwrap_or_else(|_| unreachable!()));

/// The runtime half, wrapped so a *user* turn can carry it.
///
/// The block has to travel after the conversation to keep the history inside
/// the cached prefix, and a trailing user message is the only shape every
/// provider on the OpenAI-compatible wire accepts — a second system message is
/// rejected by some and silently hoisted by others, and hoisting it would put
/// the volatile text back in front of the history, which is the exact cost this
/// avoids.
///
/// The envelope is what stops that being a lie about who is speaking. Without
/// it the model reads live state and a correction as the user's own words; with
/// it they are labelled, in the same shape tool output uses two sections above.
/// A forged delimiter is escaped for the same reason it is there — a correction
/// or a contributor section is text this module did not write.
pub fn runtime_reminder(block: &str) -> String {
    let escaped = REMINDER_DELIMITER.replace_all(block, |captures: &Captures<'_>| {
        format!("<\\{}{}", &captures[1], &captures[2])
    });
    format!("<{REMINDER_TAG}>\n{escaped}\n</{REMINDER_TAG}>")
}

/// The whole system message, from one template.
///
/// Nothing is placed for the operator here — no separator, no live-state block,
/// no tool-output policy. A template that wants one names
/// its placeholder, and a template that names none gets exactly what it says.
///
/// The section *templates* still apply: `{{platformPolicy}}` and
/// `{{toolPolicy}}` render from the agent's own overrides, so raw mode decides
/// the layout rather than throwing the wording away. `live_prompt` is the one
/// field it ignores, because its entire content is `{{time}}{{wrapUp}}` and
/// both are named here directly.
///
/// **The cache cost is real and worth stating.** In template mode the identity
/// half is a byte-identical prefix a provider discounts for the life of the
/// session. One blob rebuilt per iteration has no such prefix if anything in it
/// moves — a `{{time}}` at the top ends the discount for everything after it,
/// on every request of every turn. A raw template that names no volatile
/// placeholder renders identically each iteration and caches exactly as well as
/// before, which is the case an operator writing a fixed instruction sheet
/// lands in anyway.
pub fn build_raw_prompt(options: &BuildRawPrompt<'_>) -> String {
    let context = options.context;
    let mut live = live_values(
        context,
        options.time_zone,
        options
            .agent
            .and_then(|agent| agent.wrap_up_prompt.as_deref()),
        options.nonce,
    );

    let label = options.agent.map_or("", |agent| agent.label.as_str());
    let stored = options
        .agent
        .map_or("", |agent| agent.system_prompt.as_str());
    // Whitespace-only is empty here too, and it matters more than it does in
    // template mode: a raw agent whose template renders to nothing would be
    // sent no system message at all.
    let template = if js_trim(stored).is_empty() {
        DEFAULT_SYSTEM_PROMPT_TEMPLATE
    } else {
        stored
    };

    let runtime_sections = runtime_sections_of(options.contributors, context);
    let statics = options.static_sections.join(SECTION_SEPARATOR);
    let correction = js_trim(options.correction.unwrap_or(""));

    // The same values the live-state section carries in template mode, so a raw
    // agent and a template agent on one machine read the same clock and the
    // same iteration counter.
    live.extend(values([
        (
            "name",
            if label.is_empty() {
                "GhostAI".to_owned()
            } else {
                label.to_owned()
            },
        ),
        ("workspaceId", context.static_context.workspace_id.clone()),
        (
            "workspaceRoot",
            context.static_context.workspace_root.clone(),
        ),
        ("runtime", options.host.runtime_label.clone()),
        (
            "platformPolicy",
            command_policy(
                &options.host,
                &context.static_context.workspace_id,
                options.tools,
            ),
        ),
        // Self-contained, and with the nonce: raw mode is one blob placed by
        // the operator, so there is no cached half to keep a delimiter out of.
        // In template mode the policy's prose and the delimiter it refers to
        // are split across the two halves on purpose; here they would land in
        // the same blob anyway, and a `{{toolPolicy}}` that named no delimiter
        // would quietly stop saying what it used to — the placeholder means
        // "the tool-output policy", not "most of it".
        (
            "toolPolicy",
            options.tools.map_or_else(String::new, |tools| {
                raw_tool_policy(tools.policy_prompt.as_deref(), options.nonce)
            }),
        ),
        ("nonce", options.nonce.to_owned()),
        (
            "contributors",
            if statics.is_empty() {
                String::new()
            } else {
                format!("{SECTION_SEPARATOR}{statics}")
            },
        ),
        (
            "runtimeSections",
            if runtime_sections.is_empty() {
                String::new()
            } else {
                format!("\n\n{}", runtime_sections.join("\n\n"))
            },
        ),
        (
            "correction",
            if correction.is_empty() {
                String::new()
            } else {
                format!("\n\n{correction}")
            },
        ),
    ]));

    render_prompt_template(template, &live)
}
