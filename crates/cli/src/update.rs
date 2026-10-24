//! `darkwire update`: replace this binary with a release from GitHub.
//!
//! The steps are `install.sh`'s, with its caution. Nothing is extracted until the
//! checksum matches, and nothing replaces the running file until the new one has
//! started and named its own version. This is the only code that asks GitHub
//! anything, and it runs only when someone types the command.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_i18n::{args, keys};
use futures::StreamExt;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tokio_util::sync::CancellationToken;

use crate::i18n::{Env, Translations};
use crate::program::UpdateCommand;
use crate::{INTERRUPTED, Streams, VERSION};

/// The releases API. It names the latest tag in a field, where the web page
/// names it only in a redirect.
pub const API_BASE: &str = "https://api.github.com/repos/therezor/darkwire";

/// Where a tag's assets are downloaded from.
pub const DOWNLOAD_BASE: &str = "https://github.com/therezor/darkwire/releases/download";

/// Several times the size of any release. A body past it is not a release.
const MAX_TARBALL_BYTES: usize = 256 * 1024 * 1024;
const MAX_SUMS_BYTES: usize = 64 * 1024;
/// The release JSON carries the changelog section as its body.
const MAX_API_BYTES: usize = 1024 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Between reads rather than for the whole body, so a slow link still finishes.
const READ_TIMEOUT: Duration = Duration::from_mins(1);

/// Everything [`update`] reaches outside itself, so a test can point it at a
/// local server and a temporary directory.
pub struct UpdateOptions<'a> {
    /// What was typed.
    pub command: UpdateCommand,
    /// For the locale.
    pub env: &'a Env,
    /// [`API_BASE`], or a test server.
    pub api_base: String,
    /// [`DOWNLOAD_BASE`], or a test server.
    pub download_base: String,
    /// The file to replace, with symlinks resolved.
    pub install_path: PathBuf,
    /// `std::env::consts::OS`.
    pub os: &'a str,
    /// `std::env::consts::ARCH`.
    pub arch: &'a str,
    /// The running version.
    pub current: Version,
    /// Runs the staged binary with `--version` and returns what it printed.
    pub probe: fn(&Path) -> io::Result<String>,
}

/// The release asset's target for an OS and an architecture, as
/// `std::env::consts` names them.
///
/// The four builds `release.yml` makes. These are this binary's own
/// compile-time values, so unlike `install.sh` there is no userland to
/// second-guess.
#[must_use]
pub fn release_target(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        _ => None,
    }
}

/// Runs `darkwire update` against GitHub, replacing the running binary.
pub async fn run(command: &UpdateCommand, env: &Env, streams: &mut Streams) -> Result<u8> {
    let current = Version::parse(VERSION)
        .map_err(|error| WireError::new(ErrorKind::Internal, error.to_string()))?;
    let install_path = std::env::current_exe().and_then(std::fs::canonicalize)?;

    // Ctrl-C cancels the download rather than killing the process, so the
    // staged file is dropped and removed instead of left beside the binary.
    let cancel = CancellationToken::new();
    let interrupt = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel.cancel();
            }
        })
    };

    let result = update(
        UpdateOptions {
            command: command.clone(),
            env,
            api_base: API_BASE.to_owned(),
            download_base: DOWNLOAD_BASE.to_owned(),
            install_path,
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            current,
            probe: probe_binary,
        },
        &cancel,
        streams,
    )
    .await;
    interrupt.abort();

    match result {
        Err(error) if error.kind == ErrorKind::Aborted => Ok(INTERRUPTED),
        other => other,
    }
}

/// Looks up the release, and unless only asked to check, installs it.
pub async fn update(
    options: UpdateOptions<'_>,
    cancel: &CancellationToken,
    streams: &mut Streams,
) -> Result<u8> {
    let t = Translations::for_env(options.env, None);
    let current = &options.current;

    let Some(target) = release_target(options.os, options.arch) else {
        return Err(WireError::new(
            ErrorKind::NotFound,
            t.tr(
                keys::update::NO_BUILD,
                args!["os" => options.os, "arch" => options.arch],
            ),
        ));
    };
    // Both refusals come before anything is downloaded. `--check` writes
    // nothing, so neither applies to it.
    let staged = if options.command.check {
        None
    } else if is_source_build(&options.install_path) {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            t.tr(
                keys::update::SOURCE_BUILD,
                args!["path" => options.install_path.display().to_string()],
            ),
        ));
    } else {
        Some(stage_beside(&options.install_path, &t)?)
    };

    let client = client()?;
    let (tag, wanted) = match &options.command.tag {
        Some(tag) => {
            let tag = if tag.starts_with('v') {
                tag.clone()
            } else {
                format!("v{tag}")
            };
            let version = version_of(&tag, &t)?;
            (tag, version)
        }
        None => latest_release(&client, &options.api_base, cancel, &t).await?,
    };

    let Some(staged) = staged else {
        // `--check` cannot take a tag, so this is the latest release.
        say(streams, &compared(&t, &wanted, current))?;
        return Ok(0);
    };
    // A named tag always installs: it may be a downgrade, or a repair.
    if options.command.tag.is_none() && wanted <= *current {
        say(streams, &compared(&t, &wanted, current))?;
        return Ok(0);
    }

    let asset = format!("darkwire-{target}.tar.gz");
    say(
        streams,
        &t.tr(
            keys::update::DOWNLOADING,
            args!["asset" => asset.as_str(), "tag" => tag.as_str()],
        ),
    )?;
    // The tag rather than `latest/download`, so the checksums and the tarball
    // cannot come from two different releases.
    let base = format!("{}/{tag}", options.download_base.trim_end_matches('/'));
    let tarball = download_verified(&client, &base, &asset, cancel, &t).await?;
    say(streams, &t.t(keys::update::VERIFIED))?;

    let to = wanted.to_string();
    install(&tarball, staged, &asset, &to, &options, &t)?;
    say(
        streams,
        &t.tr(
            keys::update::UPDATED,
            args!["from" => current.to_string(), "to" => to],
        ),
    )?;
    say(streams, &t.t(keys::update::RESTART))?;
    Ok(0)
}

/// The asset's bytes, once they hash to what the release's `SHA256SUMS` lists.
async fn download_verified(
    client: &reqwest::Client,
    base: &str,
    asset: &str,
    cancel: &CancellationToken,
    t: &Translations,
) -> Result<Vec<u8>> {
    let sums_url = format!("{base}/SHA256SUMS");
    let sums = fetch(client.get(&sums_url), &sums_url, MAX_SUMS_BYTES, cancel, t).await?;
    let expected = expected_digest(&String::from_utf8_lossy(&sums), asset).ok_or_else(|| {
        WireError::new(
            ErrorKind::NotFound,
            t.tr(keys::update::NOT_IN_SUMS, args!["asset" => asset]),
        )
    })?;
    let url = format!("{base}/{asset}");
    let tarball = fetch(client.get(&url), &url, MAX_TARBALL_BYTES, cancel, t).await?;

    let actual = hex(&Sha256::digest(&tarball));
    if actual != expected {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            t.tr(
                keys::update::CHECKSUM_MISMATCH,
                args![
                    "asset" => asset,
                    "expected" => expected.as_str(),
                    "actual" => actual.as_str(),
                ],
            ),
        ));
    }
    Ok(tarball)
}

/// Extracts the binary into `staged`, starts it, and renames it over the
/// installed one if it names `version`.
fn install(
    tarball: &[u8],
    mut staged: NamedTempFile,
    asset: &str,
    version: &str,
    options: &UpdateOptions<'_>,
    t: &Translations,
) -> Result<()> {
    if !extract_binary(tarball, staged.as_file_mut())? {
        return Err(WireError::new(
            ErrorKind::NotFound,
            t.tr(keys::update::NOT_IN_ARCHIVE, args!["asset" => asset]),
        ));
    }
    make_executable(staged.as_file())?;
    // Closed before it is run: Linux refuses to execute a file that is still
    // open for writing.
    let staged = staged.into_temp_path();

    let printed = (options.probe)(&staged).map_err(|error| {
        WireError::new(
            ErrorKind::Internal,
            t.tr(
                keys::update::PROBE_FAILED,
                args!["reason" => error.to_string()],
            ),
        )
    })?;
    if printed != version {
        return Err(WireError::new(
            ErrorKind::Internal,
            t.tr(
                keys::update::PROBE_MISMATCH,
                args!["printed" => printed, "expected" => version],
            ),
        ));
    }

    // A rename onto a new inode. Writing into the running file instead is
    // ETXTBSY on Linux, and on macOS it can kill a process whose signed image
    // changed under it.
    staged.persist(&options.install_path).map_err(|failed| {
        WireError::new(
            ErrorKind::Storage,
            t.tr(
                keys::update::REPLACE_FAILED,
                args![
                    "path" => options.install_path.display().to_string(),
                    "reason" => failed.error.to_string(),
                ],
            ),
        )
    })?;
    Ok(())
}

/// `<binary> --version`, as the binary answers it.
///
/// Run before the swap, so a build this machine cannot start (a glibc older
/// than the one it links, the wrong architecture) fails here and the working
/// binary stays where it is.
///
/// # Errors
///
/// The binary did not run, or exited unsuccessfully.
pub fn probe_binary(path: &Path) -> io::Result<String> {
    let output = std::process::Command::new(path)
        .arg("--version")
        // A release build refuses to start at all when asked for test seams.
        .env_remove("DARKWIRE_TEST_HOOKS")
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// What `--check`, or an update with nothing newer to install, says.
fn compared(t: &Translations, latest: &Version, current: &Version) -> String {
    let (latest_text, current_text) = (latest.to_string(), current.to_string());
    let args = args!["latest" => latest_text.as_str(), "current" => current_text.as_str()];
    match latest.cmp(current) {
        Ordering::Greater => t.tr(keys::update::AVAILABLE, args),
        Ordering::Less => t.tr(keys::update::NEWER_THAN_LATEST, args),
        Ordering::Equal => t.tr(
            keys::update::UP_TO_DATE,
            args!["version" => current_text.as_str()],
        ),
    }
}

fn version_of(tag: &str, t: &Translations) -> Result<Version> {
    Version::parse(tag.strip_prefix('v').unwrap_or(tag)).map_err(|_| {
        WireError::new(
            ErrorKind::InvalidInput,
            t.tr(keys::update::NOT_A_TAG, args!["tag" => tag]),
        )
    })
}

/// Under cargo's `target/debug/` or `target/release/`, which `cargo build` owns.
fn is_source_build(path: &Path) -> bool {
    let mut parents = path.ancestors().skip(1).map(Path::file_name);
    let profile = parents.next().flatten();
    let target = parents.next().flatten();
    target == Some(OsStr::new("target"))
        && matches!(profile.and_then(OsStr::to_str), Some("debug" | "release"))
}

/// A file in the binary's own directory, so the swap is one rename on one
/// filesystem.
fn stage_beside(install_path: &Path, t: &Translations) -> Result<NamedTempFile> {
    let dir = install_path.parent().unwrap_or_else(|| Path::new("."));
    tempfile::Builder::new()
        .prefix(".darkwire-update-")
        .tempfile_in(dir)
        .map_err(|error| match error.kind() {
            io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => WireError::new(
                ErrorKind::PermissionDenied,
                t.tr(
                    keys::update::NOT_WRITABLE,
                    args!["dir" => dir.display().to_string()],
                ),
            )
            .with_source(error),
            _ => WireError::from(error),
        })
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        // GitHub's API refuses a request that names no user agent.
        .user_agent(concat!("darkwire/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .map_err(|error| WireError::new(ErrorKind::Internal, error.to_string()))
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

async fn latest_release(
    client: &reqwest::Client,
    api_base: &str,
    cancel: &CancellationToken,
    t: &Translations,
) -> Result<(String, Version)> {
    let url = format!("{}/releases/latest", api_base.trim_end_matches('/'));
    let request = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json");
    let body = fetch(request, &url, MAX_API_BYTES, cancel, t).await?;
    let release: Release =
        serde_json::from_slice(&body).map_err(|error| fetch_failed(t, &url, &error.to_string()))?;
    let version = version_of(&release.tag_name, t)?;
    Ok((release.tag_name, version))
}

/// The whole body, or why not. Cancelling drops the request mid-flight.
async fn fetch(
    request: reqwest::RequestBuilder,
    url: &str,
    limit: usize,
    cancel: &CancellationToken,
    t: &Translations,
) -> Result<Vec<u8>> {
    let read = async {
        let response = request
            .send()
            .await
            .map_err(|error| fetch_failed(t, url, &reason_of(&error)))?;
        let status = response.status();
        if !status.is_success() {
            return Err(fetch_failed(t, url, &status.to_string()));
        }
        let mut body = Vec::new();
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|error| fetch_failed(t, url, &reason_of(&error)))?;
            if body.len() + chunk.len() > limit {
                return Err(WireError::new(
                    ErrorKind::InvalidInput,
                    t.tr(
                        keys::update::TOO_LARGE,
                        args!["url" => url, "limit" => limit / (1024 * 1024)],
                    ),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    };
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(WireError::new(ErrorKind::Aborted, "interrupted")),
        result = read => result,
    }
}

fn fetch_failed(t: &Translations, url: &str, reason: &str) -> WireError {
    WireError::new(
        ErrorKind::Network,
        t.tr(
            keys::update::FETCH_FAILED,
            args!["url" => url, "reason" => reason],
        ),
    )
    .with_retryable(true)
}

/// reqwest's own message, then each cause under it. The top line alone is
/// "error sending request", which says nothing about DNS, TLS or a refusal.
fn reason_of(error: &reqwest::Error) -> String {
    let mut reason = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        let _ = write!(reason, ": {cause}");
        source = cause.source();
    }
    reason
}

/// The digest `SHA256SUMS` lists for `asset`, lowercased.
///
/// Both spellings of the name: `sha256sum` writes `*name` for a file it read in
/// binary mode, and `install.sh` accepts both for the same reason.
fn expected_digest(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let digest = fields.next()?;
        let name = fields.next()?;
        (name == asset || name.strip_prefix('*') == Some(asset))
            .then(|| digest.to_ascii_lowercase())
    })
}

/// Copies the archive's `<dir>/darkwire` into `into`. False when there is none.
///
/// Read entry by entry and never unpacked, so no path inside the tarball is
/// ever written to.
fn extract_binary(tarball: &[u8], into: &mut std::fs::File) -> io::Result<bool> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tarball));
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let is_binary = {
            let path = entry.path()?;
            let parts: Vec<Component<'_>> = path.components().collect();
            matches!(
                parts.as_slice(),
                [Component::Normal(_), Component::Normal(name)] if *name == "darkwire"
            )
        };
        if is_binary {
            io::copy(&mut entry, into)?;
            into.flush()?;
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(unix)]
fn make_executable(file: &std::fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(file: &std::fs::File) -> io::Result<()> {
    let _ = file;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn say(streams: &mut Streams, line: &str) -> Result<()> {
    writeln!(streams.out, "{line}")?;
    Ok(())
}
