//! The terminal as a device: how big it is, and what the keyboard just did.
//!
//! Nothing here draws. The renderer decides what the screen should say, and
//! this decides nothing at all — it turns bytes into keys, puts the tty into
//! raw mode, and puts it back. Both halves are traits so a test can hand in a
//! terminal that exists entirely in memory and read back the bytes a completed
//! paint emitted, which are the same bytes whether the machine is fast or slow.
//!
//! Raw mode is the part with teeth. With it on there is no echo and no line
//! discipline, so a process that dies without restoring it leaves a shell that
//! needs `stty sane`. Every path out of [`Keyboard::stop`] restores, `stop` is
//! idempotent, and the caller is expected to arrange for it to run on every
//! exit path — a `Drop` on the owning struct as well as the ordinary return.

use std::io::{self, IsTerminal, Read, Write};

use crate::keys::{Key, parse_keys};

const DEFAULT_COLUMNS: usize = 80;
const DEFAULT_ROWS: usize = 24;

/// Where a frame is written, and how big the window behind it is.
pub trait TerminalOutput {
    /// Writes bytes to the terminal. A frame arrives as one call.
    fn write_str(&mut self, text: &str);
    /// The window width, if the device knows it. Zero means it does not.
    fn columns(&self) -> Option<u16>;
    /// The window height, if the device knows it. Zero means it does not.
    fn rows(&self) -> Option<u16>;
    /// Whether the output is an interactive terminal rather than a pipe.
    fn is_tty(&self) -> bool;
}

/// Where keystrokes come from, and the tty mode that decides how they arrive.
pub trait TerminalInput {
    /// Whether the input is an interactive terminal rather than a pipe.
    fn is_tty(&self) -> bool;
    /// Whether the tty is already in raw mode.
    fn is_raw(&self) -> bool;
    /// Whether the mode can be set at all. A pipe has no line discipline.
    fn supports_raw_mode(&self) -> bool;
    /// Turns raw mode on or off.
    fn set_raw_mode(&mut self, raw: bool) -> io::Result<()>;
    /// One chunk of input as it arrived, or `None` at end of input.
    fn read_chunk(&mut self) -> io::Result<Option<String>>;
}

/// A reported size, or a usable number.
///
/// `columns().unwrap_or(80)` is the obvious spelling and it is wrong: a device
/// can report **zero**, which is a value, and every width then collapses to
/// nothing. It is not hypothetical — `script(1)` allocates a pty with no size,
/// so a session recorded with it would render a header of blank lines and a
/// status line consisting of one ellipsis. A terminal mid-resize can answer 0
/// as well.
fn size_of(reported: Option<u16>, fallback: usize) -> usize {
    match reported {
        Some(size) if size > 0 => usize::from(size),
        _ => fallback,
    }
}

/// How many columns wide the output is, treating 0 as "no idea". `fallback`
/// defaults to 80.
pub fn columns_of(output: &dyn TerminalOutput, fallback: Option<usize>) -> usize {
    size_of(output.columns(), fallback.unwrap_or(DEFAULT_COLUMNS))
}

/// How many rows tall the output is, treating 0 as "no idea". `fallback`
/// defaults to 24.
pub fn rows_of(output: &dyn TerminalOutput, fallback: Option<usize>) -> usize {
    size_of(output.rows(), fallback.unwrap_or(DEFAULT_ROWS))
}

/// A keyboard over an input: chunks in, decoded keys out.
///
/// Pull rather than push — the caller owns the loop, reads keys when it is
/// ready for them, and stops when it is done. That is what keeps this crate
/// free of a runtime.
pub struct Keyboard<I: TerminalInput> {
    input: I,
    /// Whether this keyboard turned raw mode on, and so owes turning it off.
    owned: bool,
    stopped: bool,
}

impl<I: TerminalInput> Keyboard<I> {
    /// The next chunk of keys, decoded. Empty at end of input, and always
    /// empty once stopped.
    pub fn read_keys(&mut self) -> io::Result<Vec<Key>> {
        if self.stopped {
            return Ok(Vec::new());
        }
        Ok(self
            .input
            .read_chunk()?
            .map(|chunk| parse_keys(&chunk))
            .unwrap_or_default())
    }

    /// Restores the tty and stops reading. Idempotent.
    pub fn stop(&mut self) -> io::Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        if self.owned {
            self.input.set_raw_mode(false)?;
        }
        Ok(())
    }

    /// Whether [`Keyboard::stop`] has run.
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// The input, for a caller that needs to look at it.
    pub fn input(&self) -> &I {
        &self.input
    }
}

impl<I: TerminalInput> Drop for Keyboard<I> {
    /// The exit path nobody wrote: whatever unwinds through here gives the tty
    /// back. An error restoring the mode has nowhere left to be reported.
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Starts reading keys, taking the tty out of line mode when asked to.
///
/// `raw` defaults to "whenever the input is a terminal". `Some(false)` is what
/// a test passes and what a pipe gets. Only a mode this turned on is turned
/// off again — toggling it underneath something else that set it is how a
/// terminal ends up with no echo.
pub fn open_keyboard<I: TerminalInput>(mut input: I, raw: Option<bool>) -> io::Result<Keyboard<I>> {
    let raw = raw.unwrap_or_else(|| input.is_tty());
    let owned = raw && !input.is_raw() && input.supports_raw_mode();
    if owned {
        input.set_raw_mode(true)?;
    }
    Ok(Keyboard {
        input,
        owned,
        stopped: false,
    })
}

/// The process's standard output as a terminal.
#[derive(Debug, Default)]
pub struct StandardOutput;

impl TerminalOutput for StandardOutput {
    fn write_str(&mut self, text: &str) {
        let mut stdout = io::stdout().lock();
        // A failed write to the terminal has no better recovery than the next
        // frame, which writes everything that changed again.
        let _ = stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush());
    }

    fn columns(&self) -> Option<u16> {
        crossterm::terminal::size().ok().map(|(columns, _)| columns)
    }

    fn rows(&self) -> Option<u16> {
        crossterm::terminal::size().ok().map(|(_, rows)| rows)
    }

    fn is_tty(&self) -> bool {
        io::stdout().is_terminal()
    }
}

/// The process's standard input as a terminal.
///
/// Reads bytes, not events, so the decoder in [`crate::keys`] is the one
/// naming keys in production as well as in tests.
#[derive(Debug, Default)]
pub struct StandardInput;

impl TerminalInput for StandardInput {
    fn is_tty(&self) -> bool {
        io::stdin().is_terminal()
    }

    fn is_raw(&self) -> bool {
        crossterm::terminal::is_raw_mode_enabled().unwrap_or(false)
    }

    fn supports_raw_mode(&self) -> bool {
        self.is_tty()
    }

    fn set_raw_mode(&mut self, raw: bool) -> io::Result<()> {
        if raw {
            crossterm::terminal::enable_raw_mode()
        } else {
            crossterm::terminal::disable_raw_mode()
        }
    }

    fn read_chunk(&mut self) -> io::Result<Option<String>> {
        let mut buffer = [0_u8; 4096];
        let read = io::stdin().lock().read(&mut buffer)?;
        if read == 0 {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&buffer[..read]).into_owned()))
    }
}
