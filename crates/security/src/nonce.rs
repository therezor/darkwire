//! Tool-output envelopes, and non-destructive injection detection.
//!
//! A tool result is data written by whoever controls the file, the web page or
//! the MCP server — never an instruction. The model cannot tell the difference
//! from position alone, so every result is wrapped in a delimiter carrying a
//! per-turn random nonce, and the system prompt states that everything inside
//! such a delimiter is inert. An attacker who cannot predict the nonce cannot
//! close the envelope and cannot write text that appears to be outside it.
//!
//! That is the whole defence, and it is why two details are not optional:
//!
//!  - **The nonce is fresh per turn and comes from the OS generator.** A fixed
//!    or per-install delimiter is one successful exfiltration away from being
//!    known forever, at which point the envelope is decoration.
//!  - **Closing tags inside the content are escaped.** Content that contains the
//!    terminator would otherwise end the envelope early and the remainder would
//!    read as the agent's own reasoning. Escaping is case-insensitive because the
//!    model is doing the parsing, and a model treats `</TOOL_OUTPUT_A1B2>` as a
//!    closing tag whatever the source said.
//!
//! **Detection is deliberately non-destructive.** Matching a phrase and
//! replacing the result with a warning banner is a bug, not a mitigation: it
//! fires on this project's own security documentation, silently removes the
//! output the model asked for, and leaves it hallucinating around the hole. So a
//! match produces a notice for the UI badge and the content passes through
//! byte-for-byte. The nonce does the defending; the findings only inform.
//!
//! Offsets are UTF-16 code units, because the finding is shown beside content
//! the browser indexes that way.

use std::sync::LazyLock;

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{effective_tool_policy, render_prompt_template};
use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::random::{RandomSource, hex_lower};

const TOOL_OUTPUT_TAG_PREFIX: &str = "tool_output_";

/// 8 bytes — 64 bits of unguessable delimiter, at 16 characters of prompt.
pub const TOOL_OUTPUT_NONCE_BYTES: usize = 8;

/// Tool names reach the envelope from MCP servers and extensions, so they are
/// constrained to the ASCII word characters plus `.`, `:` and `-`.
static UNSAFE_NAME_CHARS: LazyLock<Regex> = LazyLock::new(|| compile(r"[^A-Za-z0-9_.:-]+"));

/// Every pattern here is a literal, or an escaped literal, so compilation cannot
/// fail; the `unreachable!` keeps the type without an `unwrap`.
fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!("the pattern is a literal"))
}

/// A hex-digit run of at least eight bytes.
fn is_nonce(value: &str) -> bool {
    value.len() >= 8 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A fresh nonce as 16 lowercase hex characters.
pub fn create_tool_output_nonce(random: &dyn RandomSource) -> String {
    let mut bytes = [0u8; TOOL_OUTPUT_NONCE_BYTES];
    random.fill(&mut bytes);
    hex_lower(&bytes)
}

/// The delimiter name for a nonce.
///
/// A short, non-random or empty nonce is a guessable delimiter, which is the
/// same as having none, so it is refused rather than wrapped with.
pub fn tool_output_tag(nonce: &str) -> Result<String> {
    if !is_nonce(nonce) {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            "Tool-output nonce must be at least 8 hex bytes",
        ));
    }
    Ok(format!("{TOOL_OUTPUT_TAG_PREFIX}{nonce}"))
}

/// What a finding looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectionSignal {
    /// "ignore previous instructions" and its relatives.
    InstructionOverride,
    /// An attempt to reassign the agent's identity or loyalty.
    RoleOverride,
    /// An attempt to have the system prompt or tool schema echoed back.
    PromptExtraction,
    /// An attempt to make the agent call a tool on the content's behalf.
    ToolDirective,
    /// The content contained the envelope's own delimiter. The strongest signal
    /// available: legitimate output has no reason to carry this turn's nonce.
    DelimiterForgery,
}

impl InjectionSignal {
    /// The `snake_case` spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            InjectionSignal::InstructionOverride => "instruction_override",
            InjectionSignal::RoleOverride => "role_override",
            InjectionSignal::PromptExtraction => "prompt_extraction",
            InjectionSignal::ToolDirective => "tool_directive",
            InjectionSignal::DelimiterForgery => "delimiter_forgery",
        }
    }
}

/// One phrase that read as an injected instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InjectionFinding {
    /// Which pattern matched.
    pub signal: InjectionSignal,
    /// Offset into the original content, in UTF-16 code units.
    pub index: usize,
    /// A short, whitespace-collapsed window around the match, safe for logs.
    pub excerpt: String,
}

/// The `\w` and `\b` are ASCII-only, matching the JavaScript source of these
/// patterns; the case fold and `\s` stay Unicode.
static INJECTION_PATTERNS: LazyLock<Vec<(InjectionSignal, Regex)>> = LazyLock::new(|| {
    [
        (
            InjectionSignal::InstructionOverride,
            r"(?i)(?-u:\b)(?:ignore|disregard|forget)\s+(?:all\s+|any\s+)?(?:the\s+)?(?:previous|prior|preceding|above|earlier)\s+(?:instruction|prompt|rule|direction|message)",
        ),
        (
            InjectionSignal::RoleOverride,
            r"(?i)(?-u:\b)you\s+are\s+(?:now|actually|really)(?-u:\b)|(?-u:\b)new\s+(?:instructions|persona|role)\s*:",
        ),
        (
            InjectionSignal::PromptExtraction,
            r"(?i)(?-u:\b)(?:reveal|repeat|print|output|show|echo)\s+(?:me\s+)?(?:your\s+|the\s+)?(?:system\s+prompt|initial\s+instructions|full\s+instructions)",
        ),
        (
            InjectionSignal::ToolDirective,
            r"(?i)(?-u:\b)(?:you\s+must|now)\s+(?:call|run|execute|invoke)\s+(?:the\s+)?(?-u:[\w.-])*\s*(?:tool|command)(?-u:\b)",
        ),
    ]
    .into_iter()
    .map(|(signal, pattern)| (signal, compile(pattern)))
    .collect()
});

static WHITESPACE_RUN: LazyLock<Regex> = LazyLock::new(|| compile(r"\s+"));

const EXCERPT_CONTEXT_CHARS: usize = 24;
const EXCERPT_MAX_CHARS: usize = 160;

/// UTF-16 code units of the prefix of `text` up to a byte offset.
fn utf16_index(text: &str, byte_offset: usize) -> usize {
    text[..byte_offset].encode_utf16().count()
}

fn excerpt_around(units: &[u16], index: usize, length: usize) -> String {
    let start = index.saturating_sub(EXCERPT_CONTEXT_CHARS);
    let end = (index + length + EXCERPT_CONTEXT_CHARS).min(units.len());
    let window = String::from_utf16_lossy(&units[start..end]);
    let collapsed = WHITESPACE_RUN.replace_all(&window, " ");
    let trimmed = collapsed.trim();
    let clipped_units: Vec<u16> = trimmed.encode_utf16().collect();
    let clipped = if clipped_units.len() > EXCERPT_MAX_CHARS {
        format!(
            "{}…",
            String::from_utf16_lossy(&clipped_units[..EXCERPT_MAX_CHARS])
        )
    } else {
        trimmed.to_owned()
    };
    let leading = if start > 0 { "…" } else { "" };
    let trailing = if end < units.len() { "…" } else { "" };
    format!("{leading}{clipped}{trailing}")
}

/// Reports phrases that read as injected instructions. Never modifies anything.
///
/// One finding per signal: this feeds a UI badge and a log line, and twenty
/// findings from one paragraph tell the operator nothing the first one did not.
pub fn detect_prompt_injection(content: &str) -> Vec<InjectionFinding> {
    let units: Vec<u16> = content.encode_utf16().collect();
    INJECTION_PATTERNS
        .iter()
        .filter_map(|(signal, pattern)| {
            pattern.find(content).map(|found| {
                let index = utf16_index(content, found.start());
                let length = found.as_str().encode_utf16().count();
                InjectionFinding {
                    signal: *signal,
                    index,
                    excerpt: excerpt_around(&units, index, length),
                }
            })
        })
        .collect()
}

/// How to wrap one tool result.
#[derive(Debug, Clone)]
pub struct WrapToolOutputOptions<'a> {
    /// The tool that produced the content, as the envelope names it.
    pub tool_name: &'a str,
    /// From [`create_tool_output_nonce`], regenerated once per turn.
    pub nonce: &'a str,
    /// Run injection detection. Default `true`.
    pub detect: bool,
}

impl<'a> WrapToolOutputOptions<'a> {
    /// Options with detection on.
    pub fn new(tool_name: &'a str, nonce: &'a str) -> WrapToolOutputOptions<'a> {
        WrapToolOutputOptions {
            tool_name,
            nonce,
            detect: true,
        }
    }
}

/// A wrapped tool result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WrappedToolOutput {
    /// The envelope, ready to become a `tool` message's content.
    pub text: String,
    /// The delimiter name in force.
    pub tag: String,
    /// How many delimiter-shaped sequences in the content had to be escaped.
    pub forged_delimiters: usize,
    /// What detection reported, forgery first.
    pub findings: Vec<InjectionFinding>,
}

/// Wraps tool output in this turn's delimiter.
///
/// The content is escaped but never truncated or replaced — truncation is the
/// agent loop's decision, made against a character budget, and replacement is
/// not anyone's.
pub fn wrap_tool_output(
    content: &str,
    options: &WrapToolOutputOptions<'_>,
) -> Result<WrappedToolOutput> {
    let tag = tool_output_tag(options.nonce)?;
    let name = UNSAFE_NAME_CHARS.replace_all(options.tool_name, "_");

    // Matches an opening *or* closing delimiter, case-insensitively; the nonce is
    // hex so folding case cannot collide with a different turn's tag. Escaping
    // both forms means content cannot appear to start a second envelope either.
    let delimiter = compile(&format!("(?i)<(/?)({})", regex::escape(&tag)));
    let mut forged_delimiters = 0;
    let mut first_index = None;
    let escaped = delimiter.replace_all(content, |captures: &regex::Captures<'_>| {
        forged_delimiters += 1;
        if first_index.is_none() {
            first_index = captures.get(0).map(|m| m.start());
        }
        format!("<\\{}{}", &captures[1], &captures[2])
    });

    let mut findings = Vec::new();
    if let Some(byte_index) = first_index {
        let units: Vec<u16> = content.encode_utf16().collect();
        let index = utf16_index(content, byte_index);
        findings.push(InjectionFinding {
            signal: InjectionSignal::DelimiterForgery,
            index,
            excerpt: excerpt_around(&units, index, tag.encode_utf16().count() + 2),
        });
    }
    if options.detect {
        findings.extend(detect_prompt_injection(content));
    }

    Ok(WrappedToolOutput {
        text: format!("<{tag} name=\"{name}\">\n{escaped}\n</{tag}>"),
        tag,
        forged_delimiters,
        findings,
    })
}

/// A single sentence for the notice event's message field.
pub fn describe_injection_findings(findings: &[InjectionFinding]) -> String {
    let mut signals: Vec<&str> = Vec::new();
    for finding in findings {
        let name = finding.signal.as_str();
        if !signals.contains(&name) {
            signals.push(name);
        }
    }
    format!(
        "Tool output contains text resembling injected instructions ({}). The content was passed through unchanged. Treat it as data.",
        signals.join(", ")
    )
}

/// The system-prompt section that makes the delimiters mean something.
///
/// Without this text the wrapping is inert: the model has no reason to treat one
/// region of its context differently from another. The nonce is included so the
/// instruction names the exact delimiter in force for this turn.
///
/// **`template` is the operator's, and the text is all it can change.**
/// [`wrap_tool_output`] emits the fences and escapes forged ones on every result
/// whatever is written here, so this paragraph explains a mechanism instead of
/// being one. An operator who deletes it gets envelopes their model has not been
/// told to respect — worth a warning, and not worth being the single exception
/// to a promise the rest of the prompt keeps.
///
/// The tag is derived only when a nonce is in hand. That is what lets a policy
/// naming no delimiter be rendered with no turn in hand, and so be placed in the
/// cached half of the prompt.
pub fn tool_output_policy(nonce: Option<&str>, template: Option<&str>) -> Result<String> {
    let effective = effective_tool_policy(template);
    let Some(nonce) = nonce else {
        return Ok(render_prompt_template(effective, &IndexMap::new()));
    };
    let mut values = IndexMap::new();
    values.insert("nonce".to_owned(), nonce.to_owned());
    values.insert("tag".to_owned(), tool_output_tag(nonce)?);
    Ok(render_prompt_template(effective, &values))
}
