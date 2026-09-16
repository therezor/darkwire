//! Asking a question on a terminal, without a prompt library.
//!
//! Six questions is not worth a dependency, and the shapes wanted here are not
//! the ones a prompt library is built around: a numbered list that also accepts
//! a name, and a multi-select answered on one line rather than by ticking boxes.
//!
//! Two constraints shaped the seam, and both survive the move to Rust:
//!
//!  - **One reader for a whole command, not one per question.** A command that
//!    asks two questions — which agents, then whether to approve their boxes —
//!    cannot open a reader around each: two readers on one stdin fight over the
//!    same bytes, and the second question would be answered by whatever the
//!    first left buffered. So a command that already holds an [`Ask`] passes it
//!    down rather than letting the code below open another.
//!  - **Everything takes its streams as arguments.** The line source is a
//!    trait and the sink is passed per call, so a test drives every prompt
//!    without a terminal — and so the borrow checker never has to reconcile a
//!    long-lived prompt object with a caller that also wants to print.
//!
//! End of input is not an answer. Ctrl-D closing stdin arrives here as a
//! `WireError` of kind `aborted`, which the wizard above turns into "nothing
//! was written" — because a reader that answered every remaining question with
//! an empty line would write a configuration nobody chose.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};

use darkwire_core::{Result, WireError};
use darkwire_i18n::{args, keys};
use darkwire_tui::{Palette, palette_for};

use crate::i18n::Translations;

/// Where a typed line comes from.
///
/// A trait rather than a concrete stdin, for the reason every seam in this
/// repository is injected: the tests drive the whole wizard through a scripted
/// implementation, and the prompts under test are then the same code the binary
/// runs rather than a second copy of it.
pub trait LineReader {
    /// One line, with its terminator removed. `None` at end of input.
    fn read_line(&mut self) -> Result<Option<String>>;

    /// One line that is not echoed back to the terminal.
    ///
    /// Separate from [`LineReader::read_line`] because the difference is the
    /// device's, not the caller's: suppressing the echo means taking the tty
    /// out of line mode, which only a tty can do. An implementation with no
    /// terminal behind it answers exactly as `read_line` does.
    fn read_secret(&mut self) -> Result<Option<String>>;
}

/// The process's standard input.
///
/// The secret read takes the terminal out of line mode and assembles the line
/// from keystrokes, so nothing typed reaches the scrollback. **Where there is
/// no tty there is no line discipline to suspend**, so the read falls back to
/// an ordinary one and the value is echoed — which is the honest behaviour for
/// a pipe, and a case the wizard refuses before it gets here anyway.
#[derive(Debug, Default)]
pub struct StdinReader;

impl StdinReader {
    /// A reader over the process's own stdin.
    #[must_use]
    pub fn new() -> StdinReader {
        StdinReader
    }
}

impl LineReader for StdinReader {
    fn read_line(&mut self) -> Result<Option<String>> {
        let mut line = String::new();
        let read = std::io::stdin().lock().read_line(&mut line)?;
        if read == 0 {
            return Ok(None);
        }
        Ok(Some(line.trim_end_matches(['\n', '\r']).to_owned()))
    }

    fn read_secret(&mut self) -> Result<Option<String>> {
        use darkwire_tui::{Key, KeyName, StandardInput, TerminalInput, open_keyboard};

        if !StandardInput.supports_raw_mode() {
            return self.read_line();
        }

        // `StandardInput` is a handle rather than a resource — it opens stdin
        // per read — so constructing one here takes nothing away from anything
        // else holding one. `Keyboard` restores the tty on drop as well as on
        // `stop`, so every path out of this function gives it back.
        let mut keyboard = open_keyboard(StandardInput, Some(true))?;
        let mut typed = String::new();
        loop {
            let keys: Vec<Key> = keyboard.read_keys()?;
            if keys.is_empty() {
                keyboard.stop()?;
                return Ok(None);
            }
            for key in keys {
                match key.name {
                    KeyName::Enter => {
                        keyboard.stop()?;
                        return Ok(Some(typed));
                    }
                    KeyName::Backspace => {
                        typed.pop();
                    }
                    // Ctrl-C and Ctrl-D at a masked prompt mean the same thing
                    // they mean at any other: leave, having answered nothing.
                    KeyName::Char if key.ctrl && matches!(key.character.as_str(), "c" | "d") => {
                        keyboard.stop()?;
                        return Ok(None);
                    }
                    KeyName::Char if !key.ctrl && !key.meta => typed.push_str(&key.character),
                    _ => {}
                }
            }
        }
    }
}

/// A reader that answers from a list, which is what the tests drive.
///
/// Public because the tests are integration tests in their own crate: a double
/// that only the library could see would be a double nothing could use.
#[derive(Debug, Default)]
pub struct ScriptedReader {
    lines: std::collections::VecDeque<String>,
}

impl ScriptedReader {
    /// A reader that answers with `lines`, in order, then reports end of input.
    #[must_use]
    pub fn new<I, S>(lines: I) -> ScriptedReader
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        ScriptedReader {
            lines: lines.into_iter().map(Into::into).collect(),
        }
    }

    /// How many answers are left unread.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.lines.len()
    }
}

impl LineReader for ScriptedReader {
    fn read_line(&mut self) -> Result<Option<String>> {
        Ok(self.lines.pop_front())
    }

    fn read_secret(&mut self) -> Result<Option<String>> {
        self.read_line()
    }
}

/// The prompts, bound to one line source and one colour setting.
///
/// The sink is a per-call argument rather than a field: a caller that holds an
/// `Ask` for a whole wizard also prints headings and results between questions,
/// and one object borrowing the output stream for its whole lifetime would make
/// that impossible to express.
pub struct Ask<'a> {
    reader: &'a mut dyn LineReader,
    palette: Palette,
    t: &'a Translations,
}

impl std::fmt::Debug for Ask<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ask").finish_non_exhaustive()
    }
}

impl<'a> Ask<'a> {
    /// The prompts over one reader.
    ///
    /// `colors` is the three-state answer the colour flag produces: `None` is
    /// "nobody said", which lets the palette consult the environment.
    pub fn new(
        reader: &'a mut dyn LineReader,
        colors: Option<bool>,
        t: &'a Translations,
    ) -> Ask<'a> {
        Ask {
            reader,
            palette: palette_for(colors),
            t,
        }
    }

    /// One line, or `fallback` when the line is empty.
    ///
    /// An absent `fallback` makes an empty line an empty answer, which is how
    /// `choose_many` reads "none of them" and how the API-key question reads
    /// "this endpoint needs none".
    pub fn text(
        &mut self,
        out: &mut dyn Write,
        question: &str,
        fallback: Option<&str>,
    ) -> Result<String> {
        let fallback = fallback.unwrap_or("");
        let suffix = if fallback.is_empty() {
            String::new()
        } else {
            self.palette.dim.apply(&format!(" [{fallback}]"))
        };
        write!(out, "{question}{suffix}: ")?;
        out.flush()?;

        let answer = self.line(out)?;
        let answer = answer.trim().to_owned();
        Ok(if answer.is_empty() {
            fallback.to_owned()
        } else {
            answer
        })
    }

    /// One line that does not land in the scrollback.
    pub fn secret(&mut self, out: &mut dyn Write, question: &str) -> Result<String> {
        write!(out, "{question}: ")?;
        out.flush()?;
        let Some(answer) = self.reader.read_secret()? else {
            return Err(end_of_input());
        };
        // The terminal echoed nothing, so the cursor is still on the prompt and
        // whatever prints next would continue the line the key was typed on.
        writeln!(out)?;
        Ok(answer.trim().to_owned())
    }

    /// A numbered list. Answers with the chosen index.
    ///
    /// Accepts a name as well as a number, matched by prefix: an operator who
    /// types `ollama` has answered the question, and refusing it would be
    /// pedantry. A garbled answer re-asks rather than taking the default,
    /// because a default silently applied is invisible until much later.
    pub fn choose(
        &mut self,
        out: &mut dyn Write,
        question: &str,
        options: &[String],
        fallback_index: usize,
    ) -> Result<usize> {
        self.write_options(out, options, &[])?;
        let fallback = (fallback_index + 1).to_string();
        loop {
            let answer = self.text(out, question, Some(&fallback))?;
            if let Some(index) = index_of(&answer, options.len()) {
                return Ok(index);
            }
            if let Some(index) = named(&answer, options) {
                return Ok(index);
            }
            let notice = self
                .t
                .tr(keys::init::ENTER_NUMBER, args!["max" => options.len()]);
            writeln!(out, "{}", self.palette.yellow.apply(&format!("  {notice}")))?;
        }
    }

    /// The same numbered list, answered with any number of entries.
    ///
    /// `choose` in a loop was the other option and is worse for the question
    /// this exists for: picking six agents out of eight is six prompts and six
    /// redraws, with no way to see what has already been ticked. One line
    /// answers it.
    ///
    /// Four things it accepts, because a terminal is a place people type from
    /// habit and every one of these is somebody's habit:
    ///
    ///  - numbers, `1 3 5` or `1,3,5` — commas and spaces are the same separator
    ///  - names, `coder nano`, matched by prefix exactly as [`Ask::choose`] does
    ///  - `all`, which is what somebody who wants the lot will type first
    ///  - an empty line for none, which is also how the question is declined
    ///
    /// A garbled entry re-asks rather than silently dropping. Selecting four of
    /// five things and getting three because one was misspelt is the failure
    /// worth a second prompt: it is invisible until much later, when the agent
    /// that was supposed to exist does not.
    ///
    /// `marks` annotates a row without joining its label — `[installed]` must
    /// not be typeable as a name.
    ///
    /// The answer is sorted by index rather than by the order the entries were
    /// typed, so `install b a` and `install a b` are one run rather than two.
    pub fn choose_many(
        &mut self,
        out: &mut dyn Write,
        question: &str,
        options: &[String],
        marks: &[String],
    ) -> Result<Vec<usize>> {
        self.write_options(out, options, marks)?;
        loop {
            let answer = self.text(out, question, None)?;
            let answer = answer.trim();
            if answer.is_empty() {
                return Ok(Vec::new());
            }
            if answer.eq_ignore_ascii_case("all") {
                return Ok((0..options.len()).collect());
            }

            let mut chosen: BTreeSet<usize> = BTreeSet::new();
            let mut bad: Option<&str> = None;
            for token in answer.split([' ', '\t', ',']).filter(|t| !t.is_empty()) {
                if let Some(index) = index_of(token, options.len()) {
                    chosen.insert(index);
                    continue;
                }
                if let Some(index) = named(token, options) {
                    chosen.insert(index);
                    continue;
                }
                bad = Some(token);
                break;
            }

            match bad {
                None => return Ok(chosen.into_iter().collect()),
                Some(token) => {
                    let notice = self
                        .t
                        .tr(keys::prompt::NOT_AN_OPTION, args!["name" => token]);
                    writeln!(out, "{}", self.palette.yellow.apply(&format!("  {notice}")))?;
                }
            }
        }
    }

    /// Yes or no, in the operator's language *and* in English.
    ///
    /// The literal `y`/`n` an English-only prompt tests is an accident of the
    /// language it was written in: a German operator types `j` for ja, and a
    /// prompt that reads `J/n` and then ignores `j` is worse than one that never
    /// offered the choice. The localised letters come from the bundle.
    ///
    /// English stays accepted alongside them rather than being replaced. A
    /// terminal is a place people type from muscle memory, `y` is what a decade
    /// of other tools trained, and there is no locale where accepting it costs
    /// anything — no language's negative begins with `y`, and the localised
    /// letter is tested first regardless.
    ///
    /// **An empty line is the fallback, and the hint is not an answer.** The
    /// obvious spelling — hand the `Y/n` hint to [`Ask::text`] as its fallback
    /// and let the usual substitution happen — feeds that hint back through the
    /// prefix test below, where `y/N` begins with `y`. Pressing return at a
    /// prompt that reads `[y/N]` then answers *yes*, which is the opposite of
    /// what it says and matters most where it is used: approving a container
    /// nobody asked to approve.
    pub fn confirm(&mut self, out: &mut dyn Write, question: &str, fallback: bool) -> Result<bool> {
        let hint = self.t.t(if fallback {
            keys::prompt::YES_NO_DEFAULT_YES
        } else {
            keys::prompt::YES_NO_DEFAULT_NO
        });
        let decorated = format!(
            "{question}{}",
            self.palette.dim.apply(&format!(" [{hint}]"))
        );
        let answer = self.text(out, &decorated, None)?.to_lowercase();
        if answer.is_empty() {
            return Ok(fallback);
        }

        let yes = self.t.t(keys::prompt::YES).to_lowercase();
        let no = self.t.t(keys::prompt::NO).to_lowercase();
        if starts_with(&answer, &yes) || answer.starts_with('y') {
            return Ok(true);
        }
        if starts_with(&answer, &no) || answer.starts_with('n') {
            return Ok(false);
        }
        Ok(fallback)
    }

    /// The numbered rows above a question.
    fn write_options(
        &self,
        out: &mut dyn Write,
        options: &[String],
        marks: &[String],
    ) -> Result<()> {
        for (index, option) in options.iter().enumerate() {
            let number = self.palette.dim.apply(&format!("{:>2}", index + 1));
            let mark = match marks.get(index) {
                Some(mark) if !mark.is_empty() => format!(" {}", self.palette.dim.apply(mark)),
                _ => String::new(),
            };
            writeln!(out, "  {number}  {option}{mark}")?;
        }
        Ok(())
    }

    /// One line, with end of input reported as an abort rather than as "".
    fn line(&mut self, out: &mut dyn Write) -> Result<String> {
        let Some(line) = self.reader.read_line()? else {
            // The prompt was written and never answered, so the cursor is still
            // sitting on it.
            writeln!(out)?;
            return Err(end_of_input());
        };
        Ok(line)
    }
}

/// Ctrl-D, or a stream that ran out of answers.
///
/// `aborted` rather than `invalid_input`: nothing was typed wrongly, the person
/// left — and the caller's whole job on this path is to write nothing.
fn end_of_input() -> WireError {
    WireError::aborted("the prompt")
}

/// `answer` as a 1-based index into a list of `len`, or `None`.
///
/// A trailing fractional part is split off rather than the whole answer being
/// parsed as a float, so `2.0` is the second entry and `2.5` is not an entry at
/// all — the same answer a person would give — and no value ever makes the trip
/// through a floating-point representation on its way to being an index.
fn index_of(answer: &str, len: usize) -> Option<usize> {
    let (whole, fraction) = answer.split_once('.').unwrap_or((answer, ""));
    if fraction.bytes().any(|digit| digit != b'0') {
        return None;
    }
    let ordinal: usize = whole.parse().ok()?;
    ordinal.checked_sub(1).filter(|index| *index < len)
}

/// The first option whose label starts with `answer`, ignoring case.
fn named(answer: &str, options: &[String]) -> Option<usize> {
    if answer.is_empty() {
        return None;
    }
    let needle = answer.to_lowercase();
    options
        .iter()
        .position(|option| option.to_lowercase().starts_with(&needle))
}

/// `starts_with`, with an empty needle matching nothing.
///
/// A bundle that left `prompt.yes` blank would otherwise make every answer a
/// yes, including the empty one.
fn starts_with(answer: &str, needle: &str) -> bool {
    !needle.is_empty() && answer.starts_with(needle)
}
