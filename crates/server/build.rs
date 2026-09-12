//! Decides whether the built SPA can be compiled into the binary.
//!
//! `rust-embed` reads its folder at compile time and fails the *build* when the
//! directory is missing, so a checkout that has never run `pnpm build` could
//! not compile the server at all. `GHOSTAI_HEADLESS_BUILD=1` is the escape the
//! workspace CI sets: it emits a cfg the embed declaration is gated on, leaving
//! a server whose `/api` surface is complete and whose `GET /` is an honest
//! JSON 404.

fn main() {
    println!("cargo::rerun-if-env-changed=GHOSTAI_HEADLESS_BUILD");
    println!("cargo::rustc-check-cfg=cfg(headless_ui)");

    let headless = std::env::var("GHOSTAI_HEADLESS_BUILD").is_ok_and(|value| value == "1");
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/web/dist");
    if headless || !dist.join("index.html").is_file() {
        println!("cargo::rustc-cfg=headless_ui");
    }
}
