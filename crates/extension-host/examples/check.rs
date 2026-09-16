//! `cargo run -p darkwire-extension-host --example check --features testkit -- <dir>`
//!
//! The conformance suite as a command. An extension author runs it against
//! their own directory and gets the host's verdict without writing a line of
//! Rust; the example in this repository runs it from a vitest test, which is
//! how a JavaScript author reaches it with no toolchain of their own.
//!
//! This is also the shape the operator command `darkwire extension check <id>`
//! will take when the CLI lands — the same call, against an installed id
//! instead of a path.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command's output is its whole purpose"
)]

use std::path::PathBuf;
use std::process::ExitCode;

use darkwire_extension_host::testkit::{Expect, extension_conformance};

/// `--tools N`, `--commands N`, `--context N`, in any order, after the path.
fn parse(args: &[String]) -> Option<(PathBuf, Expect)> {
    let mut rest = args.iter();
    let dir = PathBuf::from(rest.next()?);
    let mut expect = Expect::default();
    while let Some(flag) = rest.next() {
        let value: usize = rest.next()?.parse().ok()?;
        match flag.as_str() {
            "--tools" => expect.tools = value,
            "--commands" => expect.commands = value,
            "--context" => expect.context = value,
            _ => return None,
        }
    }
    Some((dir, expect))
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((dir, expect)) = parse(&args) else {
        eprintln!("usage: check <extension dir> [--tools N] [--commands N] [--context N]");
        return ExitCode::from(2);
    };

    match extension_conformance(&dir, expect).await {
        Ok(report) => {
            println!("{}", report.summary());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{}", error.message);
            ExitCode::FAILURE
        }
    }
}
