//! The `darkwire` binary.
//!
//! Everything this file does is set an exit code. The work lives in the library
//! beside it, which is what lets the whole parser and every command be driven
//! from `tests/` without a process — and what keeps this file short enough to
//! be read in one sitting.
//!
//! `anyhow` is allowed here and nowhere else in the tree: below this line every
//! failure is a `WireError`, which carries a kind a caller can branch on. Here
//! there is no caller left, only a number.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use darkwire::i18n::Env;
use darkwire::{Streams, run};

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            // Before the logger, before the translations, before anything: if
            // there is no reactor there is nothing to report it with.
            eprintln!("✖ Could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    let env = Env::from_process();
    let mut streams = Streams::process();
    let code = runtime.block_on(run(std::env::args_os(), &env, &mut streams));
    ExitCode::from(code)
}
