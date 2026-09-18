//! `AgentEvent` to a terminal.
//!
//! This is the first consumer of the event stream, and it is deliberately the
//! dumbest one possible: a `match` over the event union that writes strings.
//! There is no CLI-shaped variant of the loop and no token callback, because
//! the WebSocket hub and the Telegram renderer are the same `match` over the
//! same union. If rendering needed anything the events do not carry, that is a
//! missing field on the event, not a reason for a second path out of the loop.
//!
//! What the renderer owns, and why each is here rather than in the loop:
//!
//!  - **Line discipline.** Assistant text streams in arbitrary chunks that may
//!    or may not end on a newline, and a tool card printed straight after one
//!    would land mid-sentence. [`TurnRenderer`] tracks the cursor so a break is
//!    emitted exactly when one is needed, and never twice.
//!  - **Colour as an injected boolean.** [`darkwire_tui::palette_for`] answers
//!    with the same shape and identity styles when colour is off, so tests
//!    assert on the text rather than on escape sequences, and `--no-color` is
//!    one flag rather than a branch at every call site.
//!  - **Tool output is previewed, not printed.** A result is capped before it
//!    reaches here; dumping that into a terminal buries the answer that follows
//!    it. The first few lines are what tells a human whether the call did what
//!    they expected.
//!
//! The one thing this must *not* do is render the nonce delimiters.
//! `tool.result` carries the tool's own output for exactly that reason — the
//! envelope is a defence mechanism aimed at a language model, and showing it to
//! a human would be displaying the lock rather than the door.

use std::collections::HashMap;

use darkwire_agent::AgentEvent;
use darkwire_core::TurnStatsRecord;
use darkwire_i18n::{args, keys};
use darkwire_protocol::{
    ErrorCode, NestedAgentEvent, StopReason, SubagentEventBody, ToolRisk, TurnTiming, Usage,
    turn_rate,
};
use darkwire_tui::{Palette, Style, palette_for, strip_ansi};
use serde_json::Value;

use crate::i18n::Translations;

/// Anything that takes a string. Standard output satisfies it; so does a test.
///
/// A trait of its own rather than [`std::io::Write`] because a renderer has
/// nothing to do with a write failure: the turn is streaming, the consumer is a
/// terminal or a pipe, and unwinding an answer half-written because a pager
/// closed would lose more than it reports. Implementations swallow the error,
/// and the blanket implementation below does exactly that for every writer.
pub trait RenderTarget: Send {
    /// Writes `text`, dropping any failure.
    fn write(&mut self, text: &str);
}

/// Every writer is a render target, with a failed write dropped.
impl<W: std::io::Write + Send> RenderTarget for W {
    fn write(&mut self, text: &str) {
        let _ = std::io::Write::write_all(self, text.as_bytes());
    }
}

/// How many lines of a tool result are previewed when nobody said.
pub const DEFAULT_TOOL_RESULT_LINES: usize = 6;

/// How many characters a tool card's argument summary gets.
pub const DEFAULT_ARG_SUMMARY_CHARS: usize = 96;

/// How many characters one *value* inside that summary gets.
const VALUE_CHARS: usize = 44;

/// How many characters one previewed line of a tool result gets.
const RESULT_LINE_CHARS: usize = 100;

/// How to build a [`TurnRenderer`].
pub struct TurnRendererOptions {
    /// Where the turn is drawn.
    pub out: Box<dyn RenderTarget>,
    /// `None` detects: `NO_COLOR`, `FORCE_COLOR`, `TERM` and whether standard
    /// output is a terminal. A caller with no opinion passes nothing rather
    /// than guessing, which is the whole of the `--no-color` design.
    pub colors: Option<bool>,
    /// Reasoning deltas are dimmed rather than hidden.
    pub show_reasoning: bool,
    /// The dim token and timing line after a turn.
    ///
    /// Called stats rather than usage, which is what it was: `Usage` is the
    /// token record the protocol carries, and this is the *line* — which nobody
    /// at a prompt calls usage. `/output stats off` is how it is turned off,
    /// and one word for one thing is worth the rename.
    pub show_stats: bool,
    /// Lines of a tool result to preview. `0` prints none.
    pub tool_result_lines: usize,
    /// The terminal's translations.
    pub t: Translations,
}

impl TurnRendererOptions {
    /// The options a plain terminal turn wants: colour detected, reasoning and
    /// stats shown, six lines of every tool result, English.
    #[must_use]
    pub fn new(out: Box<dyn RenderTarget>) -> TurnRendererOptions {
        TurnRendererOptions {
            out,
            colors: None,
            show_reasoning: true,
            show_stats: true,
            tool_result_lines: DEFAULT_TOOL_RESULT_LINES,
            t: Translations::default(),
        }
    }
}

impl std::fmt::Debug for TurnRendererOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnRendererOptions")
            .field("colors", &self.colors)
            .field("show_reasoning", &self.show_reasoning)
            .field("show_stats", &self.show_stats)
            .field("tool_result_lines", &self.tool_result_lines)
            .finish_non_exhaustive()
    }
}

/// Ellipsis included in the budget, so a clipped string never exceeds `max`.
///
/// The budget is counted in UTF-16 units, which is what every other length in
/// this repository means and what the fixtures were written against. Truncation
/// still stops on a character boundary, so an astral-plane character is dropped
/// whole rather than split into halves a terminal cannot draw.
#[must_use]
pub fn clip(text: &str, max: usize) -> String {
    let flat = collapse_whitespace(text);
    if utf16_len(&flat) <= max {
        return flat;
    }

    let budget = max.saturating_sub(1);
    let mut out = String::with_capacity(flat.len());
    let mut used = 0usize;
    for character in flat.chars() {
        let width = character.len_utf16();
        if used + width > budget {
            break;
        }
        out.push(character);
        used += width;
    }
    out.push('…');
    out
}

/// Every run of whitespace as one space, with the ends removed.
///
/// A multi-line value has to stay on one line: a tool card is one row, and a
/// path containing a newline would otherwise take the card apart.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The length a terminal budget is measured in.
fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// One JSON value as its own text.
///
/// Serialisation cannot fail for a [`Value`]: the only failure `serde_json` has
/// here is a non-finite float, and a parsed value cannot hold one.
fn json_text(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// One argument's value, short enough to sit on a card.
fn format_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        other => clip(&json_text(other), VALUE_CHARS),
    }
}

/// The model's arguments on one line.
///
/// `path="src" recursive=true` rather than the raw JSON: a tool card is
/// scanned, not read, and the braces and quotes are the part carrying no
/// information. The arguments may also be a bare string — `tool.call` falls
/// back to the raw text when a model emits invalid JSON, and that case is
/// exactly the one worth showing verbatim.
///
/// A `null` is nothing rather than the word: JSON has one spelling for absence
/// where the original union had two, and a card reading `⚙ read null`
/// would be reporting the encoding rather than the call.
#[must_use]
pub fn summarise_args(args: &Value, max: usize) -> String {
    match args {
        Value::Null => String::new(),
        Value::String(raw) => clip(raw, max),
        Value::Object(entries) => {
            if entries.is_empty() {
                return String::new();
            }
            let pairs: Vec<String> = entries
                .iter()
                .map(|(key, value)| format!("{key}={}", format_value(value)))
                .collect();
            clip(&pairs.join(" "), max)
        }
        other => clip(&json_text(other), max),
    }
}

/// A duration, in the terminal's denser wording.
///
/// The hour branch is not symmetry with the web's own formatter for its own
/// sake: without it a three-hour turn rendered as `180m 00s`, which is a number
/// a reader has to divide before it means anything.
///
/// The sub-minute form stays one decimal all the way to sixty seconds, where
/// the web drops the decimal above ten. That divergence is deliberate and is
/// the same one that makes this file render `1.2k` where the web renders
/// `8,192`: a terminal line is read at a glance and a settings panel is read on
/// purpose.
#[must_use]
pub fn format_duration(ms: f64) -> String {
    if ms < 1000.0 {
        return format!("{}ms", ms.round());
    }
    if ms < 60_000.0 {
        return format!("{:.1}s", ms / 1000.0);
    }

    let total_minutes = (ms / 60_000.0).floor();
    if total_minutes < 60.0 {
        let seconds = (ms % 60_000.0 / 1000.0).round();
        return format!("{total_minutes}m {seconds:02}s");
    }

    let hours = (total_minutes / 60.0).floor();
    let minutes = total_minutes % 60.0;
    format!("{hours}h {minutes:02}m")
}

/// A token count, rounded to thousands once it stops being readable at a
/// glance.
#[allow(
    clippy::cast_precision_loss,
    reason = "token counts are far below 2^53"
)]
#[must_use]
pub fn format_count(value: u64) -> String {
    if value < 1000 {
        value.to_string()
    } else {
        format!("{:.1}k", value as f64 / 1000.0)
    }
}

/// Completion tokens per second, as a phrase, or nothing.
///
/// Takes the whole timing bag rather than one number because the divisor is a
/// choice — generation time where a provider reported it, whole-turn wall clock
/// where it did not — and [`turn_rate`] owns that choice for both this and the
/// browser. It reports nothing for a turn that produced no tokens or was
/// measured at zero milliseconds, and that stays unreported here: a rate
/// derived from a zero is a number that looks measured and is not.
#[must_use]
pub fn format_rate(usage: &Usage, timing: &TurnTiming) -> Option<String> {
    turn_rate(usage, timing).map(|rate| format!("{rate:.1} tok/s"))
}

/// What a turn cost, as one phrase.
fn format_usage(usage: &Usage) -> String {
    let mut parts = vec![
        format!("{} in", format_count(usage.prompt_tokens)),
        format!("{} out", format_count(usage.completion_tokens)),
    ];
    if let Some(cached) = usage.cached_tokens.filter(|count| *count > 0) {
        parts.push(format!("{} cached", format_count(cached)));
    }
    if let Some(reasoning) = usage.reasoning_tokens.filter(|count| *count > 0) {
        parts.push(format!("{} reasoning", format_count(reasoning)));
    }
    parts.join(" / ")
}

/// Why a turn stopped, for a human.
///
/// `complete` is absent on purpose: the answer is already on screen, and
/// announcing that it finished normally is noise on every single turn.
fn stop_reason_key(reason: StopReason) -> Option<&'static str> {
    match reason {
        StopReason::Complete => None,
        StopReason::Aborted => Some(keys::render::stop_reasons::ABORTED),
        StopReason::MaxIterations => Some(keys::render::stop_reasons::MAX_ITERATIONS),
        StopReason::WallTimeout => Some(keys::render::stop_reasons::WALL_TIMEOUT),
        StopReason::Error => Some(keys::render::stop_reasons::ERROR),
    }
}

/// The wire spelling of an error code.
///
/// Matched rather than derived from the serialiser, because this is the string
/// an operator reads beside the sentence and then searches the documentation
/// for. It has to be the same word every other surface prints.
fn error_code(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Unauthorized => "unauthorized",
        ErrorCode::BadRequest => "bad_request",
        ErrorCode::NotFound => "not_found",
        ErrorCode::RateLimited => "rate_limited",
        ErrorCode::ProviderError => "provider_error",
        ErrorCode::ToolError => "tool_error",
        ErrorCode::ConfigInvalid => "config_invalid",
        ErrorCode::NotConfigured => "not_configured",
        ErrorCode::SessionBusy => "session_busy",
        ErrorCode::Internal => "internal",
    }
}

/// The colour a tool card is badged in.
///
/// This stays here rather than moving to `darkwire-tui` with the rest of the
/// palette: it takes a [`ToolRisk`], and a crate whose whole claim is that it
/// has never heard of an agent cannot be the one that knows `exec` is red.
fn risk_style(colors: &Palette, risk: ToolRisk) -> Style {
    match risk {
        ToolRisk::Exec => colors.red,
        ToolRisk::Network => colors.magenta,
        ToolRisk::Write => colors.yellow,
        ToolRisk::Safe => colors.cyan,
    }
}

/// Which of the two streams is currently being written.
///
/// Told apart by the break between them rather than by a header. Reasoning
/// carried a `┄ thinking` label, on the argument that dimmed prose is
/// indistinguishable from the answer on a terminal that renders dim as plain.
/// In practice it read as a label on something that does not need one: the
/// reasoning arrives before the answer, ends at a line break, and is the only
/// dim run in a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Nothing is streaming.
    Idle,
    /// The answer.
    Assistant,
    /// The model's reasoning.
    Reasoning,
}

/// One turn, drawn onto a terminal.
pub struct TurnRenderer {
    out: Box<dyn RenderTarget>,
    colors: Palette,
    /// Both flags are settable, because `/output` turns them off part way
    /// through a session. The flags that set them at launch — `--no-reasoning`,
    /// and `--json`, which suppresses the lot — are the same two fields; a REPL
    /// simply gets to change its mind.
    show_reasoning: bool,
    show_stats: bool,
    tool_result_lines: usize,
    t: Translations,
    /// Tool name by call, so a result can label itself without re-reading.
    ///
    /// Keyed by `session:call` rather than by the call id alone. A call id is
    /// the model's and is only unique within one assistant message, so a
    /// subagent can mint the same one its caller just used — and a shared map
    /// would then have the child's result deleting the parent's label.
    calls: HashMap<String, String>,
    at_line_start: bool,
    mode: Mode,
    /// The session the current top-level turn is on. Set by `turn.start`.
    session_key: String,
    /// How far in to write. `0` for the operator's own turn.
    ///
    /// Indentation is the only hierarchy signal a terminal has, and this file
    /// already used it for tool results — so a subagent is two more spaces
    /// rather than a new idea. Applied in the one place text reaches the
    /// stream, so streamed prose is indented on every line it wraps onto and
    /// not only where a line happens to start.
    depth: usize,
}

impl std::fmt::Debug for TurnRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnRenderer")
            .field("show_reasoning", &self.show_reasoning)
            .field("show_stats", &self.show_stats)
            .field("depth", &self.depth)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl TurnRenderer {
    /// A renderer over one target.
    #[must_use]
    pub fn new(options: TurnRendererOptions) -> TurnRenderer {
        TurnRenderer {
            out: options.out,
            colors: palette_for(options.colors),
            show_reasoning: options.show_reasoning,
            show_stats: options.show_stats,
            tool_result_lines: options.tool_result_lines,
            t: options.t,
            calls: HashMap::new(),
            at_line_start: true,
            mode: Mode::Idle,
            session_key: String::new(),
            depth: 0,
        }
    }

    /// Draws one event.
    pub fn handle(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Subagent(body) => self.subagent(body),
            // The terminal has the same figure in its header, and refreshes it
            // at the same turn boundaries `/context` measures at — so it has
            // the staleness this event exists to fix, just less visibly than a
            // bar sitting under a composer. What stops wiring it up being free
            // is that the header redraws the whole prompt line, and doing that
            // mid-stream is the thing `darkwire-tui` was written to make safe
            // rather than something to bolt on here.
            AgentEvent::ContextUsage(_) => {}
            AgentEvent::Nested(nested) => {
                let session_key = self.session_key.clone();
                self.render(nested, &session_key);
            }
        }
    }

    /// One event, at the depth already set, attributed to `session_key`.
    ///
    /// Split from [`Self::handle`] so a subagent's events go through exactly
    /// the same rendering as its caller's — a nested `exec` looks like an
    /// `exec`, which is the whole point of indentation being the only
    /// difference.
    fn render(&mut self, event: &NestedAgentEvent, session_key: &str) {
        match event {
            NestedAgentEvent::TurnStart(start) => {
                self.mode = Mode::Idle;
                // Only the operator's own turn resets the map. A subagent's
                // `turn.start` arriving here would otherwise drop the labels of
                // the calls its caller has in flight — including the delegating
                // call itself.
                if self.depth == 0 {
                    self.session_key.clone_from(&start.session_key);
                    self.calls.clear();
                }
            }
            NestedAgentEvent::AssistantDelta(delta) => {
                self.stream(Mode::Assistant, &delta.text);
            }
            NestedAgentEvent::ReasoningDelta(delta) => {
                if self.show_reasoning {
                    self.stream(Mode::Reasoning, &delta.text);
                }
            }
            NestedAgentEvent::ToolCall(call) => {
                self.tool_call(
                    session_key,
                    &call.call_id,
                    &call.name,
                    &call.args,
                    call.risk,
                );
            }
            NestedAgentEvent::ToolProgress(progress) => {
                let name = self
                    .calls
                    .get(&call_key(session_key, &progress.call_id))
                    .cloned()
                    .unwrap_or_else(|| "tool".to_owned());
                let elapsed = format_duration(to_ms(progress.elapsed_ms));
                let text = self.colors.dim.apply(&format!("  … {name} {elapsed}"));
                self.line(&text);
            }
            NestedAgentEvent::ToolResult(result) => {
                self.tool_result(session_key, result);
            }
            // The terminal has no way to answer one — `darkwire chat` installs
            // no gate, so an `ask` tool simply runs. Reaching here means the
            // CLI is watching a turn some other surface is driving, and saying
            // so beats a gap.
            NestedAgentEvent::ToolApprovalRequest(request) => {
                let sentence = self.t.tr(
                    keys::render::AWAITING_APPROVAL,
                    args!["tool" => request.name.as_str()],
                );
                let text = self.colors.yellow.apply(&format!("⧗ {sentence}"));
                self.line(&text);
            }
            NestedAgentEvent::Notice(notice) => {
                let mark = self.colors.yellow.apply("⚠");
                let body = self.colors.yellow.apply(&notice.message);
                self.line(&format!("{mark} {body}"));
            }
            NestedAgentEvent::Error(error) => {
                self.error(error_code(error.code), &error.message, error.retryable);
            }
            NestedAgentEvent::TurnEnd(end) => {
                // The event itself is the timing bag, so there is nothing to
                // assemble beyond widening the integers it carries.
                let timing = TurnTiming {
                    generation_ms: end.generation_ms.map(to_ms),
                    generation_tokens: end.generation_tokens.map(to_ms),
                    elapsed_ms: end.elapsed_ms.map(to_ms),
                };
                self.turn_end(end.stop_reason, end.iterations, end.usage.as_ref(), &timing);
            }
        }
    }

    /// A subagent's event, indented under the call that started it.
    ///
    /// The two ends of the delegated turn get a rule of their own, at the
    /// *parent's* depth, because they are the boundary rather than something
    /// inside it — the same shape a mode change uses. Everything between
    /// renders one level in, through the same rendering the caller's events
    /// use.
    fn subagent(&mut self, event: &SubagentEventBody) {
        let who = if event.label.is_empty() {
            event.agent_id.clone()
        } else {
            event.label.clone()
        };
        let previous = self.depth;
        let depth = usize::try_from(event.depth).unwrap_or(0);

        if matches!(event.event, NestedAgentEvent::TurnStart(_)) {
            self.break_line();
            self.mode = Mode::Idle;
            self.depth = depth.saturating_sub(1);
            let sentence = self
                .t
                .tr(keys::render::subagent::START, args!["agent" => who]);
            let text = self.colors.dim.apply(&format!("┄ {sentence}"));
            self.line(&text);
            self.depth = previous;
            return;
        }

        self.depth = depth;
        self.render(&event.event, &event.session_key);
        self.depth = previous;

        if !matches!(event.event, NestedAgentEvent::TurnEnd(_)) {
            return;
        }

        self.depth = depth.saturating_sub(1);
        let sentence = self
            .t
            .tr(keys::render::subagent::DONE, args!["agent" => who]);
        let text = self.colors.dim.apply(&format!("┄ {sentence}"));
        self.line(&text);
        self.depth = previous;
    }

    /// Ends the turn's last line, so a prompt is never printed onto it.
    pub fn finish(&mut self) {
        self.break_line();
        self.mode = Mode::Idle;
    }

    /// A line of the CLI's own, in the same line discipline as the events.
    pub fn note(&mut self, text: &str) {
        let line = self.colors.dim.apply(text);
        self.line(&line);
    }

    /// The operator's own message, printed into the transcript.
    ///
    /// The editor holds the line while it is being typed and clears it on
    /// Return, so nothing would otherwise record what was asked — the frame is
    /// not the transcript. It is not an `AgentEvent` and deliberately does not
    /// go through the match: nothing on the wire says "a human pressed Return
    /// in a terminal", and inventing an event so that one renderer could draw a
    /// caret would be putting a CLI concern into a union three transports
    /// share.
    ///
    /// The blank line above it is the one line of space between one exchange
    /// and the next. With no prompt to carry a leading newline it has to be
    /// written, and here is the only place that knows a new exchange is
    /// starting.
    pub fn echo(&mut self, text: &str) {
        self.break_line();
        self.write("\n");
        // The same caret the editor draws, in the same colour: scrolling back
        // through a long session, these are what the eye counts exchanges by.
        let caret = self.colors.green.apply("›");
        self.line(&format!("{caret} {text}"));
    }

    /// A diagnostic from somewhere else, put into the transcript intact.
    ///
    /// Logs reach the terminal through the same sink the turn's text does — a
    /// line written straight to the descriptor would land wherever the cursor
    /// happens to be, which while an answer is streaming is the middle of a
    /// word. Routing it here is only half the fix; the other half is this line
    /// break. Measured without it:
    /// `- **Edit{"level":40,…,"msg":"mcp server unavailable"} files**`.
    ///
    /// The text keeps its own newline and its own shape. It is not this
    /// renderer's to reformat, and a structured log line that has been prettied
    /// is a log line that no longer matches what is in the file.
    pub fn aside(&mut self, text: &str) {
        self.break_line();
        self.write(text);
    }

    /// Something the operator should notice, in the CLI's own voice.
    pub fn warn(&mut self, text: &str) {
        let mark = self.colors.yellow.apply("⚠");
        self.line(&format!("{mark} {text}"));
    }

    /// Whether the model's reasoning is streamed.
    #[must_use]
    pub fn reasoning_shown(&self) -> bool {
        self.show_reasoning
    }

    /// Shows or hides the model's reasoning from here on.
    pub fn set_reasoning_shown(&mut self, shown: bool) {
        self.show_reasoning = shown;
    }

    /// Whether the token and timing line is printed after a turn.
    #[must_use]
    pub fn stats_shown(&self) -> bool {
        self.show_stats
    }

    /// Shows or hides the token and timing line from here on.
    pub fn set_stats_shown(&mut self, shown: bool) {
        self.show_stats = shown;
    }

    /// What past turns cost, one line each.
    ///
    /// Through the renderer rather than written straight to the stream, like
    /// every other output: this object owns the cursor position, and a command
    /// that wrote around it would put the next prompt on the end of a line.
    #[allow(
        clippy::cast_precision_loss,
        reason = "token counts and millisecond stamps are far below 2^53"
    )]
    pub fn stats(&mut self, rows: &[TurnStatsRecord]) {
        for row in rows {
            let elapsed = (row.ended_at_ms - row.started_at_ms).max(0) as f64;
            let mut parts = vec![
                if row.model.is_empty() {
                    "unknown model".to_owned()
                } else {
                    row.model.clone()
                },
                self.t
                    .tr(keys::render::STEPS, args!["count" => row.iterations]),
                format_usage(&row.usage),
                format_duration(elapsed),
            ];
            let timing = TurnTiming {
                generation_ms: row.generation_ms.map(|value| value as f64),
                generation_tokens: row.generation_tokens.map(|value| value as f64),
                elapsed_ms: Some(elapsed),
            };
            if let Some(rate) = format_rate(&row.usage, &timing) {
                parts.push(rate);
            }
            let line = self.colors.dim.apply(&format!("  · {}", parts.join(" · ")));
            self.line(&line);
        }
    }

    /// Assistant text and reasoning, told apart by the break between them.
    fn stream(&mut self, mode: Mode, text: &str) {
        if self.mode != mode {
            self.break_line();
            self.mode = mode;
        }
        if mode == Mode::Reasoning {
            let dimmed = self.colors.dim.apply(text);
            self.write(&dimmed);
        } else {
            self.write(text);
        }
    }

    fn tool_call(
        &mut self,
        session_key: &str,
        call_id: &str,
        name: &str,
        args: &Value,
        risk: ToolRisk,
    ) {
        self.calls
            .insert(call_key(session_key, call_id), name.to_owned());
        let style = risk_style(&self.colors, risk);
        let summary = summarise_args(args, DEFAULT_ARG_SUMMARY_CHARS);
        let head = format!("{} {}", style.apply("⚙"), style.apply(name));
        let line = if summary.is_empty() {
            head
        } else {
            format!("{head} {}", self.colors.dim.apply(&summary))
        };
        self.line(&line);
    }

    fn tool_result(&mut self, session_key: &str, result: &darkwire_protocol::ToolResult) {
        let mark = if result.ok {
            self.colors.green.apply("✓")
        } else {
            self.colors.red.apply("✗")
        };
        let suffix = if result.truncated { ", truncated" } else { "" };
        let timing = self.colors.dim.apply(&format!(
            "{}{suffix}",
            format_duration(to_ms(result.duration_ms))
        ));
        self.line(&format!("  {mark} {timing}"));
        self.calls.remove(&call_key(session_key, &result.call_id));

        if self.tool_result_lines == 0 || result.content.is_empty() {
            return;
        }
        let lines: Vec<&str> = result.content.split('\n').collect();
        for line in lines.iter().take(self.tool_result_lines) {
            let text = self
                .colors
                .dim
                .apply(&format!("    {}", clip(line, RESULT_LINE_CHARS)));
            self.line(&text);
        }
        let hidden = lines.len().saturating_sub(self.tool_result_lines);
        if hidden > 0 {
            let text = self.colors.dim.apply(&format!("    … {hidden} more lines"));
            self.line(&text);
        }
    }

    fn error(&mut self, code: &str, message: &str, retryable: bool) {
        let mark = self.colors.red.apply("✖");
        self.line(&format!("{mark} {message}"));
        let retry = if retryable { " · retryable" } else { "" };
        let text = self.colors.dim.apply(&format!("  {code}{retry}"));
        self.line(&text);
    }

    fn turn_end(
        &mut self,
        stop_reason: StopReason,
        iterations: u64,
        usage: Option<&Usage>,
        timing: &TurnTiming,
    ) {
        self.break_line();
        self.mode = Mode::Idle;

        // `complete` has no key, so a turn that finished normally prints
        // nothing rather than announcing itself on every single turn.
        if let Some(key) = stop_reason_key(stop_reason) {
            let text = self.colors.yellow.apply(&format!("  {}", self.t.t(key)));
            self.line(&text);
        }
        if !self.show_stats {
            return;
        }

        let steps = i64::try_from(iterations).unwrap_or(i64::MAX);
        let mut parts = vec![self.t.tr(keys::render::STEPS, args!["count" => steps])];
        if let Some(usage) = usage.filter(|usage| usage.total_tokens > 0) {
            parts.push(format_usage(usage));
        }
        if let Some(elapsed) = timing.elapsed_ms {
            parts.push(format_duration(elapsed));
        }
        if let Some(rate) = usage.and_then(|usage| format_rate(usage, timing)) {
            parts.push(rate);
        }
        let line = self.colors.dim.apply(&format!("  · {}", parts.join(" · ")));
        self.line(&line);
    }

    fn line(&mut self, text: &str) {
        self.break_line();
        self.write(&format!("{text}\n"));
    }

    /// A newline only when the cursor is not already at the start of one.
    fn break_line(&mut self) {
        if !self.at_line_start {
            self.write("\n");
        }
    }

    /// The one place text reaches the stream, and therefore the one place
    /// indent belongs.
    ///
    /// Indenting per line would be simpler and wrong: assistant text arrives as
    /// arbitrary chunks, so a subagent's answer would be indented on whichever
    /// line a chunk happened to start and flush left on every line it wrapped
    /// onto. Rewriting newlines here catches both.
    fn write(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let indent = "  ".repeat(self.depth);
        if indent.is_empty() {
            self.out.write(text);
        } else {
            self.out.write(&indented(text, &indent, self.at_line_start));
        }
        // Measured on what a reader sees, not on the bytes. Dimmed reasoning
        // ends in a closing sequence however its prose ended, so testing the
        // raw string reports "mid-line" for a chunk that plainly finished one —
        // and the next break then writes a newline nobody asked for.
        self.at_line_start = strip_ansi(text).ends_with('\n');
    }
}

/// Unique per call across nesting. See [`TurnRenderer::calls`].
fn call_key(session_key: &str, call_id: &str) -> String {
    format!("{session_key}:{call_id}")
}

/// A millisecond count as the timing bag carries it.
#[allow(
    clippy::cast_precision_loss,
    reason = "millisecond stamps are far below 2^53"
)]
fn to_ms(value: u64) -> f64 {
    value as f64
}

/// `text` with `indent` at the start of every line it writes.
///
/// `at_line_start` decides whether the *first* line gets one — a chunk landing
/// mid-sentence must not be pushed across. A trailing newline is left bare, so
/// an indent is never written onto a line nothing has been put on yet: it would
/// become trailing whitespace the moment the next chunk is a newline of its
/// own.
fn indented(text: &str, indent: &str, at_line_start: bool) -> String {
    let broken = format!("\n{indent}");
    let body = text.replace('\n', &broken);
    let trimmed = body
        .strip_suffix(indent)
        .filter(|_| body.ends_with(&broken))
        .unwrap_or(&body);
    if at_line_start {
        format!("{indent}{trimmed}")
    } else {
        trimmed.to_owned()
    }
}
