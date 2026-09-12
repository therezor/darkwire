//! Decides whether the built SPA can be compiled into the binary.
//!
//! `rust-embed` reads its folder at compile time, so the bundle is a build
//! input rather than a path resolved at startup. Two things stand in for it,
//! and both have to be asked for: `GHOSTAI_HEADLESS_BUILD=1`, which is what the
//! workspace CI sets to run the Rust tests without a web build first, and
//! `--no-default-features`, which drops `embed-ui` altogether. With neither, a
//! missing `packages/web/dist` is an error naming the command that produces it.
//!
//! It is an error rather than a quiet fallback because the two failures are not
//! comparable. A build that stops says one sentence to the person who can fix
//! it in one command; a build that silently drops the UI ships a binary whose
//! `GET /` is an honest JSON 404 to someone who asked for a browser, with
//! nothing between here and there that would notice.

fn main() {
    println!("cargo::rerun-if-env-changed=GHOSTAI_HEADLESS_BUILD");
    println!("cargo::rustc-check-cfg=cfg(headless_ui)");

    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/web/dist");
    let index = dist.join("index.html");

    // Rerun when the bundle appears, changes or goes away. Without this, a
    // decision taken on a checkout that had no bundle survives in a cached
    // `target/` after `pnpm build` has run — which is exactly the order a CI
    // cache restore produces.
    println!("cargo::rerun-if-changed={}", dist.display());
    println!("cargo::rerun-if-changed={}", index.display());

    let headless = std::env::var("GHOSTAI_HEADLESS_BUILD").is_ok_and(|value| value == "1");
    let embedding = std::env::var_os("CARGO_FEATURE_EMBED_UI").is_some();

    if headless || !embedding {
        println!("cargo::rustc-cfg=headless_ui");
        return;
    }

    if !index.is_file() {
        println!(
            "cargo::error=the web bundle is missing: {} does not exist. \
             Run `pnpm build` first, or set GHOSTAI_HEADLESS_BUILD=1 to build a server with no UI.",
            index.display()
        );
    }
}
