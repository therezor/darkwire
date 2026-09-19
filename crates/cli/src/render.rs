//! `AgentEvent` to a terminal.
//!
//! This is the first consumer of the event stream, and it is deliberately the
//! dumbest one possible: a `match` over the event union that emits
//! [`TranscriptEvent`]s. There is no CLI-shaped variant of the loop and no
//! token callback, because the WebSocket hub and the Telegram renderer are the
//! same `match` over the same union. If rendering needed anything the events do
//! not carry, that is a missing field on the event, not a reason for a second
//! path out of the loop.
//!
//! What the renderer owns, and why each is here rather than in the loop:
//!
//!  - **Kinds, not bytes.** Every line goes out as what it is, and every
//!    boundary as itself. A consumer that can fold knows where a reasoning run
//!    ended because it was told, rather than by guessing from where a write
//!    happened to land, and the blank row above an exchange is a rule read off
//!    [`LineKind`] rather than a newline somebody remembered to write.
//!  - **Line discipline belongs to the consumer.** Assistant text streams in
//!    arbitrary chunks that may or may not end on a newline, so something has
//!    to decide when a break is owed. That is a question about what has already
//!    been drawn, which is [`PlainPrinter`]'s to answer for a stream and a
//!    frame's to answer for its rows.
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
use darkwire_protocol::tasks::TaskStatus;
use darkwire_protocol::{
    ErrorCode, NestedAgentEvent, StopReason, SubagentEventBody, ToolRisk, TurnTiming, Usage,
    turn_rate,
};
use darkwire_tui::{Palette, Style, palette_for, strip_ansi};
use serde_json::Value;

use crate::i18n::Translations;

/// One thing a turn produced, as its own kind rather than as bytes.
///
/// The renderer used to write strings and announce four boundaries alongside
/// them, which meant a fold was a guess about where a write had landed. A
/// consumer now gets the kind with the text, so where a reasoning run ends is
/// a fact rather than an inference, and blank rows are a layout rule the
/// printer owns instead of newlines somebody remembered to emit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptEvent {
    /// A chunk of the answer. May start or end mid-line.
    AssistantDelta {
        /// The text, exactly as the model produced it.
        text: String,
        /// How far in it belongs. `0` for the operator's own turn.
        depth: usize,
    },
    /// A chunk of the model's reasoning, already dimmed.
    ReasoningDelta {
        /// The text, styled.
        text: String,
        /// How far in it belongs.
        depth: usize,
    },
    /// Whatever line is open has ended.
    ///
    /// A stream arrives in chunks that need not end on a newline, so something
    /// has to say when one is finished. Saying it as an event rather than by
    /// writing a newline is what lets a pipe add the break and a surface
    /// holding rows simply close the row it had open.
    EndLine,
    /// The model has started reasoning; what follows is that run.
    ReasoningStart,
    /// The reasoning run has ended. Whatever follows is not part of it.
    ReasoningEnd,
    /// One complete line, styled and indented, ready to print.
    Line {
        /// What the line is, which is what decides the space around it.
        kind: LineKind,
        /// The line itself, with no trailing newline.
        text: String,
    },
    /// A tool's output follows, under `summary`.
    ToolBodyStart {
        /// The row that says how the call went, and the row a fold leaves
        /// behind when the output under it is hidden.
        summary: String,
    },
    /// The tool's output has ended.
    ToolBodyEnd,
    /// The rows announcing a plan, for a surface that does not keep one.
    TasksCard {
        /// The card, one line per task, already indented.
        card: String,
    },
    /// The plan is now this. Only ever after a call that succeeded.
    ///
    /// Separate from [`TranscriptEvent::TasksCard`], and the separation is the
    /// point: a `todo` call that is refused still announces itself, and a
    /// surface that painted its plan from the announcement would show a plan
    /// nothing is running.
    Tasks(Vec<(TaskStatus, String)>),
    /// What the turn cost, as the row that says so.
    ///
    /// Built whatever the switch says. A surface that can fold keeps the row
    /// and hides it; one that only writes bytes reads `shown` and drops it. A
    /// row that was never built is a row no key can reveal.
    TurnStats {
        /// The row itself.
        line: String,
        /// Whether a target that only writes bytes prints it.
        shown: bool,
    },
    /// The reasoning channel has been switched on or off for this session.
    ///
    /// `/output reasoning off` stops the run reaching a consumer at all, which
    /// a surface drawing a fold has to know: without it, a key that unfolds the
    /// reasoning would keep claiming there was some.
    ReasoningShown(bool),
    /// The turn's cost has been switched on or off for this session.
    StatsShown(bool),
}

/// What a complete line is, which is what decides the space around it.
///
/// The printer reads this instead of looking for blank lines in the text. That
/// is the whole reason it exists: spacing that lives in the string is spacing
/// nothing can adjust once the string has been built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// The operator's own message, which opens an exchange.
    Echo,
    /// A tool call announcing itself.
    ToolCall,
    /// Anything in the CLI's own voice: a note, a warning, an error.
    Notice,
    /// A boundary rule around a delegated turn.
    Subagent,
    /// A diagnostic from somewhere else, kept exactly as it arrived.
    Aside,
}

/// Anywhere a turn can be drawn. Standard output satisfies it; so does a test.
///
/// A trait of its own rather than [`std::io::Write`] because a renderer has
/// nothing to do with a write failure: the turn is streaming, the consumer is a
/// terminal or a pipe, and unwinding an answer half-written because a pager
/// closed would lose more than it reports. Implementations swallow the error.
pub trait TranscriptSink: Send {
    /// Takes one event, dropping any failure.
    fn emit(&mut self, event: TranscriptEvent);
}

/// A sink that drops everything, for a caller with nowhere to draw.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullSink;

impl TranscriptSink for NullSink {
    fn emit(&mut self, event: TranscriptEvent) {
        let _ = event;
    }
}

/// Any writer, taking the byte stream a pipe would have seen.
///
/// The printer is held here rather than reached for per call, because the line
/// discipline is state: whether a newline is owed before the next line depends
/// on what the last one ended with.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlainSink<W> {
    out: W,
    printer: PlainPrinter,
}

impl<W: std::io::Write + Send> PlainSink<W> {
    /// A sink writing into `out`.
    #[must_use]
    pub fn new(out: W) -> PlainSink<W> {
        PlainSink {
            out,
            printer: PlainPrinter::new(),
        }
    }
}

impl<W: std::io::Write + Send> TranscriptSink for PlainSink<W> {
    fn emit(&mut self, event: TranscriptEvent) {
        let bytes = self.printer.bytes(&event);
        if bytes.is_empty() {
            return;
        }
        let _ = self.out.write_all(bytes.as_bytes());
    }
}

/// Turns events back into the byte stream a pipe has always seen.
///
/// The line discipline lives here rather than in the renderer, and that is the
/// point of the split: whether a newline is needed before the next line is a
/// question about what has been printed, which only the thing printing knows.
/// A frame asks the same question of its own rows and gets a different answer.
#[derive(Clone, Copy, Debug)]
pub struct PlainPrinter {
    /// Whether the cursor sits at the start of a line.
    at_line_start: bool,
}

/// A stream nothing has been written to yet, which is a line start.
///
/// Spelled out rather than derived: `bool::default()` is `false`, which would
/// make a fresh printer think it owed a newline and open every stream with a
/// blank row.
impl Default for PlainPrinter {
    fn default() -> PlainPrinter {
        PlainPrinter::new()
    }
}

impl PlainPrinter {
    /// A printer for a stream nothing has been written to yet.
    #[must_use]
    pub fn new() -> PlainPrinter {
        PlainPrinter {
            at_line_start: true,
        }
    }

    /// The bytes `event` becomes on a stream that cannot fold anything.
    pub fn bytes(&mut self, event: &TranscriptEvent) -> String {
        match event {
            TranscriptEvent::AssistantDelta { text, depth }
            | TranscriptEvent::ReasoningDelta { text, depth } => self.stream(text, *depth),
            TranscriptEvent::Line { kind, text } => {
                // The one line of space between one exchange and the next. It
                // is a rule about what an echo is, not a newline the renderer
                // has to remember to write.
                let gap = if *kind == LineKind::Echo { "\n" } else { "" };
                self.whole(&format!("{gap}{text}\n"))
            }
            TranscriptEvent::ToolBodyStart { summary } => self.whole(summary),
            TranscriptEvent::TasksCard { card } => self.whole(card),
            TranscriptEvent::TurnStats { line, shown } => {
                if *shown {
                    self.whole(&format!("{line}\n"))
                } else {
                    String::new()
                }
            }
            // A boundary ends whatever line is open. The two streams are told
            // apart by the break between them rather than by a label, so the
            // break is what the boundary means on a flat stream.
            TranscriptEvent::EndLine
            | TranscriptEvent::ReasoningStart
            | TranscriptEvent::ReasoningEnd => self.whole(""),
            TranscriptEvent::ToolBodyEnd
            | TranscriptEvent::Tasks(_)
            | TranscriptEvent::ReasoningShown(_)
            | TranscriptEvent::StatsShown(_) => String::new(),
        }
    }

    /// Something that has to start on a line of its own.
    ///
    /// The break belongs here rather than at the source: whether one is owed is
    /// a question about what has already been printed, and a surface drawing
    /// rows instead of bytes answers it differently.
    fn whole(&mut self, text: &str) -> String {
        let break_first = if self.at_line_start { "" } else { "\n" };
        self.at_line_start = true;
        format!("{break_first}{text}")
    }

    /// `text` at `depth`, continuing whatever line is open.
    fn stream(&mut self, text: &str, depth: usize) -> String {
        if text.is_empty() {
            return String::new();
        }
        let indent = "  ".repeat(depth);
        let body = if indent.is_empty() {
            text.to_owned()
        } else {
            indented(text, &indent, self.at_line_start)
        };
        // Measured on what a reader sees, not on the bytes. Dimmed reasoning
        // ends in a closing sequence however its prose ended, so testing the
        // raw string reports "mid-line" for a chunk that plainly finished one.
        self.at_line_start = strip_ansi(text).ends_with('\n');
        body
    }
}

/// How many lines of a tool result are previewed when nobody said.
pub const DEFAULT_TOOL_RESULT_LINES: usize = 6;

/// How many characters a tool card's argument summary gets.
pub const DEFAULT_ARG_SUMMARY_CHARS: usize = 96;

/// The one tool this file renders specially. See [`task_lines`].
const TODO_TOOL: &str = "todo";

/// How many characters one *value* inside that summary gets.
const VALUE_CHARS: usize = 44;

/// How many characters one previewed line of a tool result gets.
const RESULT_LINE_CHARS: usize = 100;

/// How to build a [`TurnRenderer`].
pub struct TurnRendererOptions {
    /// Where the turn is drawn.
    pub out: Box<dyn TranscriptSink>,
    /// `None` detects: `NO_COLOR`, `FORCE_COLOR`, `TERM` and whether standard
    /// output is a terminal. A caller with no opinion passes nothing rather
    /// than guessing, which is the whole of the `--no-color` design.
    pub colors: Option<bool>,
    /// Reasoning deltas are dimmed rather than hidden.
    pub show_reasoning: bool,
    /// The dim token and timing line after a turn.
    ///
    /// Called stats rather than usage, which is what it was: `Usage` is the
    /// token record the protocol carries, and this is the *line*, which nobody
    /// at a prompt calls usage. `/output stats` and `ctrl-y` are how it is
    /// turned on, and one word for one thing is worth the rename.
    ///
    /// It decides whether a target that only writes bytes prints the row. The
    /// row is built either way, so a frame can hold one it is not showing.
    pub show_stats: bool,
    /// Lines of a tool result to preview. `0` prints none.
    pub tool_result_lines: usize,
    /// The terminal's translations.
    pub t: Translations,
}

impl TurnRendererOptions {
    /// The options a plain terminal turn wants: colour detected, reasoning
    /// shown, what the turn cost folded away, six lines of every tool result,
    /// English.
    ///
    /// `show_stats` is `false` here because it is `false` in the product, and a
    /// constructor that disagreed with the shipped default would be a second
    /// answer to the same question.
    #[must_use]
    pub fn new(out: Box<dyn TranscriptSink>) -> TurnRendererOptions {
        TurnRendererOptions {
            out,
            colors: None,
            show_reasoning: true,
            show_stats: false,
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

/// The task list a `todo` call carries, one line per task.
///
/// The only per-tool branch in this file, and it earns the exception the way the
/// terminal-styled `exec` output does: the argument summary reads
/// `tasks=[{"text":"Inspect auth",…}]` clipped at eighty characters, which is a
/// tool card reporting its own encoding instead of the plan. A plan is the one
/// argument anybody watching a turn actually wants to read.
///
/// Returns the rows as plain text with the markers in front. Colour is applied
/// by the caller, which is what keeps this testable with `--no-color`'s
/// behaviour and no palette at all.
///
/// An empty list, or arguments that are not a list of tasks, returns nothing —
/// the caller then falls back to the ordinary summary rather than printing a
/// heading over nothing.
#[must_use]
pub fn task_lines(args: &Value) -> Vec<(TaskStatus, String)> {
    let Some(Value::Array(tasks)) = args.get("tasks") else {
        return Vec::new();
    };
    tasks
        .iter()
        .filter_map(|task| {
            let text = task.get("text").and_then(Value::as_str)?;
            if text.is_empty() {
                return None;
            }
            let status = task
                .get("status")
                .and_then(Value::as_str)
                .and_then(TaskStatus::parse)
                .unwrap_or_default();
            Some((status, text.to_owned()))
        })
        .collect()
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
    out: Box<dyn TranscriptSink>,
    colors: Palette,
    /// Both flags are settable, because `/output` turns them off part way
    /// through a session. The flags that set them at launch — `--no-reasoning`,
    /// and `--json`, which suppresses the lot — are the same two fields; a REPL
    /// simply gets to change its mind.
    show_reasoning: bool,
    show_stats: bool,
    tool_result_lines: usize,
    t: Translations,
    /// The plan a `todo` call asked for, until its result says whether it stuck.
    ///
    /// Keyed like [`TurnRenderer::calls`], for the same reason.
    pending_tasks: HashMap<String, Vec<(TaskStatus, String)>>,
    /// Tool name by call, so a result can label itself without re-reading.
    ///
    /// Keyed by `session:call` rather than by the call id alone. A call id is
    /// the model's and is only unique within one assistant message, so a
    /// subagent can mint the same one its caller just used — and a shared map
    /// would then have the child's result deleting the parent's label.
    calls: HashMap<String, String>,
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
            pending_tasks: HashMap::new(),
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
                self.set_mode(Mode::Idle);
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
                self.line(LineKind::Notice, &text);
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
                self.line(LineKind::Notice, &text);
            }
            NestedAgentEvent::Notice(notice) => {
                let mark = self.colors.yellow.apply("⚠");
                let body = self.colors.yellow.apply(&notice.message);
                self.line(LineKind::Notice, &format!("{mark} {body}"));
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
            self.set_mode(Mode::Idle);
            self.depth = depth.saturating_sub(1);
            let sentence = self
                .t
                .tr(keys::render::subagent::START, args!["agent" => who]);
            let text = self.colors.dim.apply(&format!("┄ {sentence}"));
            self.line(LineKind::Subagent, &text);
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
        self.line(LineKind::Subagent, &text);
        self.depth = previous;
    }

    /// Ends the turn's last line, so a prompt is never printed onto it.
    pub fn finish(&mut self) {
        self.set_mode(Mode::Idle);
    }

    /// A line of the CLI's own, in the same line discipline as the events.
    pub fn note(&mut self, text: &str) {
        // Out of the reasoning run first. The CLI's own voice is not the
        // model's thinking, and a note folded away inside it is a note nobody
        // asked to hide.
        self.set_mode(Mode::Idle);
        let line = self.colors.dim.apply(text);
        self.line(LineKind::Notice, &line);
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
        self.set_mode(Mode::Idle);
        // The same caret the editor draws, in the same colour: scrolling back
        // through a long session, these are what the eye counts exchanges by.
        // The blank row above it is the printer's, decided by the kind.
        let caret = self.colors.green.apply("›");
        self.line(LineKind::Echo, &format!("{caret} {text}"));
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
    /// The text keeps its own shape. It is not this renderer's to reformat, and
    /// a structured log line that has been prettied is a log line that no
    /// longer matches what is in the file. Its trailing newline is the
    /// printer's, like every other line's, so it is trimmed here rather than
    /// sent twice.
    pub fn aside(&mut self, text: &str) {
        self.set_mode(Mode::Idle);
        self.line(LineKind::Aside, text.trim_end_matches('\n'));
    }

    /// Something the operator should notice, in the CLI's own voice.
    pub fn warn(&mut self, text: &str) {
        self.set_mode(Mode::Idle);
        let mark = self.colors.yellow.apply("⚠");
        self.line(LineKind::Notice, &format!("{mark} {text}"));
    }

    /// Puts a plan where the surface keeps one, without printing it.
    ///
    /// For `/tasks`, which prints the list itself and still has to move the one
    /// above the box you type into: the command's own output goes through
    /// `note` as every command's does, and this carries the same list to
    /// whatever is drawing a frame. A target that only writes bytes takes the
    /// empty card and writes nothing, which is why the two do not double up.
    pub fn plan(&mut self, tasks: &[(TaskStatus, String)]) {
        self.out.emit(TranscriptEvent::Tasks(tasks.to_vec()));
    }

    /// Whether the model's reasoning is streamed.
    #[must_use]
    pub fn reasoning_shown(&self) -> bool {
        self.show_reasoning
    }

    /// Shows or hides the model's reasoning from here on.
    pub fn set_reasoning_shown(&mut self, shown: bool) {
        self.show_reasoning = shown;
        self.out.emit(TranscriptEvent::ReasoningShown(shown));
    }

    /// Whether the token and timing line is printed after a turn.
    #[must_use]
    pub fn stats_shown(&self) -> bool {
        self.show_stats
    }

    /// Shows or hides the token and timing line from here on.
    pub fn set_stats_shown(&mut self, shown: bool) {
        self.show_stats = shown;
        self.out.emit(TranscriptEvent::StatsShown(shown));
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
            self.line(LineKind::Notice, &line);
        }
    }

    /// Moves between the answer, the reasoning and neither.
    ///
    /// The one place the mode changes, so the signals that mark a reasoning run
    /// cannot be emitted from some paths and not others. That matters for
    /// `warn` and `aside`: both reset the mode, and a warning or a log line
    /// landing inside a folded run of reasoning is a warning nobody sees.
    fn set_mode(&mut self, mode: Mode) {
        if self.mode == mode {
            return;
        }
        if self.mode != Mode::Idle {
            self.out.emit(TranscriptEvent::EndLine);
        }
        if self.mode == Mode::Reasoning {
            self.out.emit(TranscriptEvent::ReasoningEnd);
        }
        self.mode = mode;
        if mode == Mode::Reasoning {
            self.out.emit(TranscriptEvent::ReasoningStart);
        }
    }

    /// Assistant text and reasoning, told apart by the break between them.
    fn stream(&mut self, mode: Mode, text: &str) {
        let opening = self.mode != mode;
        self.set_mode(mode);
        // A provider routinely opens a channel with `"\n\nLet me think"`. The
        // mode change above already put the cursor at the start of a line, so
        // those newlines are blank rows between a message and the answer to it,
        // and with reasoning hidden nothing later collapses them. Only at the
        // start of a run: a break inside one is a paragraph somebody wrote.
        let text = if opening {
            text.trim_start_matches('\n')
        } else {
            text
        };
        // A chunk that was nothing but newlines. Returning here rather than
        // falling through, because a style applied to an empty string is still
        // an opener and a closer, and `write` would read that as a chunk that
        // finished mid-line.
        if text.is_empty() {
            return;
        }
        let depth = self.depth;
        if mode == Mode::Reasoning {
            // After the trim, or the escape prefix defeats it.
            let dimmed = self.colors.dim.apply(text);
            self.out.emit(TranscriptEvent::ReasoningDelta {
                text: dimmed,
                depth,
            });
        } else {
            self.out.emit(TranscriptEvent::AssistantDelta {
                text: text.to_owned(),
                depth,
            });
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
        let head = format!("{} {}", style.apply("⚙"), style.apply(name));

        let tasks = if name == TODO_TOOL {
            task_lines(args)
        } else {
            Vec::new()
        };
        if !tasks.is_empty() {
            let mut card = self.indent_line(&head);
            for (status, text) in &tasks {
                let row = self.task_line(*status, text);
                card.push_str(&self.indent_line(&row));
            }
            self.out.emit(TranscriptEvent::TasksCard { card });
            // Held until the result says the call worked. Only the top-level
            // plan is held at all: a subagent keeps its own list in its own
            // session, and hoisting it would overwrite the plan the operator is
            // watching with the plan of something it delegated to.
            if self.depth == 0 {
                self.pending_tasks
                    .insert(call_key(session_key, call_id), tasks);
            }
            return;
        }

        let summary = summarise_args(args, DEFAULT_ARG_SUMMARY_CHARS);
        let line = if summary.is_empty() {
            head
        } else {
            format!("{head} {}", self.colors.dim.apply(&summary))
        };
        self.line(LineKind::ToolCall, &line);
    }

    /// One task, marked and coloured by where it has got to.
    ///
    /// Done is struck through as well as ticked, because the list is read at a
    /// glance and the eye should land on the one line that is in hand. Doing is
    /// the only bold row for the same reason.
    fn task_line(&self, status: TaskStatus, text: &str) -> String {
        match status {
            TaskStatus::Done => format!(
                "  {} {}",
                self.colors.green.apply("✓"),
                self.colors
                    .dim
                    .apply(&self.colors.strikethrough.apply(text))
            ),
            TaskStatus::Doing => {
                format!(
                    "  {} {}",
                    self.colors.cyan.apply("▸"),
                    self.colors.bold.apply(text)
                )
            }
            TaskStatus::Todo => format!(
                "  {} {}",
                self.colors.dim.apply("☐"),
                self.colors.dim.apply(text)
            ),
        }
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
        let head = self.indent_line(&format!("  {mark} {timing}"));
        self.out
            .emit(TranscriptEvent::ToolBodyStart { summary: head });
        let was = self.calls.remove(&call_key(session_key, &result.call_id));

        // The plan the call asked for, now that it is known to have landed.
        if let Some(tasks) = self
            .pending_tasks
            .remove(&call_key(session_key, &result.call_id))
            && result.ok
        {
            self.out.emit(TranscriptEvent::Tasks(tasks));
        }

        // The plan was printed as the call went out, and the result is a
        // sentence counting what is already on screen.
        if was.as_deref() == Some(TODO_TOOL) && result.ok {
            self.out.emit(TranscriptEvent::ToolBodyEnd);
            return;
        }

        if self.tool_result_lines == 0 || result.content.is_empty() {
            self.out.emit(TranscriptEvent::ToolBodyEnd);
            return;
        }
        let lines: Vec<&str> = result.content.split('\n').collect();
        for line in lines.iter().take(self.tool_result_lines) {
            let text = self
                .colors
                .dim
                .apply(&format!("    {}", clip(line, RESULT_LINE_CHARS)));
            self.line(LineKind::Notice, &text);
        }
        let hidden = lines.len().saturating_sub(self.tool_result_lines);
        if hidden > 0 {
            let text = self.colors.dim.apply(&format!("    … {hidden} more lines"));
            self.line(LineKind::Notice, &text);
        }
        self.out.emit(TranscriptEvent::ToolBodyEnd);
    }

    fn error(&mut self, code: &str, message: &str, retryable: bool) {
        let mark = self.colors.red.apply("✖");
        self.line(LineKind::Notice, &format!("{mark} {message}"));
        let retry = if retryable { " · retryable" } else { "" };
        let text = self.colors.dim.apply(&format!("  {code}{retry}"));
        self.line(LineKind::Notice, &text);
    }

    fn turn_end(
        &mut self,
        stop_reason: StopReason,
        iterations: u64,
        usage: Option<&Usage>,
        timing: &TurnTiming,
    ) {
        self.set_mode(Mode::Idle);

        // `complete` has no key, so a turn that finished normally prints
        // nothing rather than announcing itself on every single turn.
        if let Some(key) = stop_reason_key(stop_reason) {
            let text = self.colors.yellow.apply(&format!("  {}", self.t.t(key)));
            self.line(LineKind::Notice, &text);
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
        // Built whatever the switch says, and handed over with it. A surface
        // that can fold keeps the row and hides it; one that only writes bytes
        // drops it. A line that was never built is a line no key can reveal.
        let line = self.indent_line(&self.colors.dim.apply(&format!("  · {}", parts.join(" · "))));
        self.out.emit(TranscriptEvent::TurnStats {
            line,
            shown: self.show_stats,
        });
    }

    /// One complete line, indented for its depth and sent as itself.
    ///
    /// No break before it and no newline after it. A line is a line here, and
    /// whether the thing printing owes a newline first is the printer's
    /// question, not this one's.
    fn line(&mut self, kind: LineKind, text: &str) {
        let indent = "  ".repeat(self.depth);
        let body = if indent.is_empty() {
            text.to_owned()
        } else {
            indented(text, &indent, true)
        };
        self.out.emit(TranscriptEvent::Line { kind, text: body });
    }

    /// One line indented for its depth, for the rows that travel inside an
    /// event of their own rather than as a line.
    fn indent_line(&self, text: &str) -> String {
        let indent = "  ".repeat(self.depth);
        if indent.is_empty() {
            format!("{text}\n")
        } else {
            indented(&format!("{text}\n"), &indent, true)
        }
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
pub(crate) fn indented(text: &str, indent: &str, at_line_start: bool) -> String {
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
