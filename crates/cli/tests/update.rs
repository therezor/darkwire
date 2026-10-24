//! `darkwire update` against a local release server and a temporary install.
//!
//! The rule every failure case checks is `install.sh`'s: whatever went wrong,
//! the binary that was there is still there, byte for byte, and nothing was left
//! beside it.

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use darkwire::Streams;
use darkwire::i18n::Env;
use darkwire::program::UpdateCommand;
use darkwire::update::{UpdateOptions, probe_binary, release_target, update};
use darkwire_core::{ErrorKind, WireError};
use darkwire_providers::testkit::{ScriptedResponse, ScriptedServer};
use flate2::Compression;
use flate2::write::GzEncoder;
use semver::Version;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

const ASSET: &str = "darkwire-x86_64-unknown-linux-gnu.tar.gz";
const OLD: &[u8] = b"the binary that was here";
const NEW: &[u8] = b"the binary from the release";

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<u8>>>);

impl Recorder {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl io::Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A gzipped tarball holding `entries`, laid out as `release.yml` lays it out.
fn tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for (path, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append_data(&mut header, path, *body).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn release(body: &[u8]) -> Vec<u8> {
    tarball(&[
        (
            "darkwire-v0.7.0-x86_64-unknown-linux-gnu/README.md",
            b"readme",
        ),
        ("darkwire-v0.7.0-x86_64-unknown-linux-gnu/darkwire", body),
    ])
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, byte| {
            write!(out, "{byte:02x}").unwrap();
            out
        })
}

fn sums_for(tarball: &[u8]) -> String {
    format!(
        "{}  darkwire-aarch64-apple-darwin.tar.gz\n{}  {ASSET}\n",
        "0".repeat(64),
        digest(tarball)
    )
}

fn latest(tag: &str) -> ScriptedResponse {
    ScriptedResponse::json(200, &json!({ "tag_name": tag, "name": tag }))
}

fn text(body: impl Into<Vec<u8>>) -> ScriptedResponse {
    ScriptedResponse::raw(200, "application/octet-stream", body)
}

/// A directory with an installed binary in it.
struct Install {
    dir: tempfile::TempDir,
    path: PathBuf,
}

impl Install {
    fn new() -> Install {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("darkwire");
        std::fs::write(&path, OLD).unwrap();
        Install { dir, path }
    }

    fn bytes(&self) -> Vec<u8> {
        std::fs::read(&self.path).unwrap()
    }

    /// Anything an update staged and did not clean up.
    fn leftovers(&self) -> Vec<String> {
        std::fs::read_dir(self.dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "darkwire")
            .collect()
    }

    fn assert_untouched(&self) {
        assert_eq!(self.bytes(), OLD, "the installed binary changed");
        assert!(
            self.leftovers().is_empty(),
            "left behind: {:?}",
            self.leftovers()
        );
    }
}

fn options<'a>(
    server: &ScriptedServer,
    install_path: &Path,
    command: UpdateCommand,
    env: &'a Env,
) -> UpdateOptions<'a> {
    UpdateOptions {
        command,
        env,
        api_base: format!("http://127.0.0.1:{}/repo", server.port()),
        download_base: format!("http://127.0.0.1:{}/download", server.port()),
        install_path: install_path.to_path_buf(),
        os: "linux",
        arch: "x86_64",
        current: Version::new(0, 6, 0),
        probe: |_| Ok("0.7.0".to_owned()),
    }
}

async fn updated(options: UpdateOptions<'_>) -> (Result<u8, WireError>, String) {
    updated_with(options, &CancellationToken::new()).await
}

async fn updated_with(
    options: UpdateOptions<'_>,
    cancel: &CancellationToken,
) -> (Result<u8, WireError>, String) {
    let out = Recorder::default();
    let mut streams = Streams {
        out: Box::new(out.clone()),
        err: Box::new(io::sink()),
    };
    let result = update(options, cancel, &mut streams).await;
    (result, out.text())
}

fn paths(server: &ScriptedServer) -> Vec<String> {
    server.calls().into_iter().map(|call| call.path).collect()
}

fn check() -> UpdateCommand {
    UpdateCommand {
        tag: None,
        check: true,
    }
}

fn tagged(tag: &str) -> UpdateCommand {
    UpdateCommand {
        tag: Some(tag.to_owned()),
        check: false,
    }
}

#[tokio::test]
async fn installs_a_newer_release_in_place_of_the_running_binary() {
    let server = ScriptedServer::start().await;
    let tarball = release(NEW);
    server
        .push(latest("v0.7.0"))
        .push(text(sums_for(&tarball)))
        .push(text(tarball));
    let install = Install::new();
    let env = Env::empty();

    let (result, out) = updated(options(
        &server,
        &install.path,
        UpdateCommand::default(),
        &env,
    ))
    .await;

    assert_eq!(result.unwrap(), 0);
    assert_eq!(install.bytes(), NEW);
    assert!(install.leftovers().is_empty(), "{:?}", install.leftovers());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&install.path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    // The tag, never `latest/download`: the checksums and the tarball have to
    // come from the same release.
    assert_eq!(
        paths(&server),
        [
            "/repo/releases/latest".to_owned(),
            "/download/v0.7.0/SHA256SUMS".to_owned(),
            format!("/download/v0.7.0/{ASSET}"),
        ]
    );
    assert!(out.contains("Checksum verified."), "{out}");
    assert!(out.contains("Updated darkwire 0.6.0 to 0.7.0."), "{out}");
    assert!(out.contains("darkwire serve"), "{out}");
}

#[tokio::test]
async fn names_itself_to_github_which_refuses_a_request_that_does_not() {
    let server = ScriptedServer::start().await;
    server.push(latest("v0.6.0"));
    let install = Install::new();
    let env = Env::empty();

    let _ = updated(options(
        &server,
        &install.path,
        UpdateCommand::default(),
        &env,
    ))
    .await;

    let call = &server.calls()[0];
    assert_eq!(
        call.headers.get("user-agent").map(String::as_str),
        Some(concat!("darkwire/", env!("CARGO_PKG_VERSION")))
    );
    assert_eq!(
        call.headers.get("accept").map(String::as_str),
        Some("application/vnd.github+json")
    );
}

#[tokio::test]
async fn an_install_that_is_up_to_date_downloads_nothing() {
    let server = ScriptedServer::start().await;
    server.push(latest("v0.6.0"));
    let install = Install::new();
    let env = Env::empty();

    let (result, out) = updated(options(
        &server,
        &install.path,
        UpdateCommand::default(),
        &env,
    ))
    .await;

    assert_eq!(result.unwrap(), 0);
    assert!(out.contains("darkwire 0.6.0 is up to date."), "{out}");
    assert_eq!(paths(&server), ["/repo/releases/latest"]);
    install.assert_untouched();
}

#[tokio::test]
async fn never_downgrades_to_the_latest_release_unless_asked_to() {
    let server = ScriptedServer::start().await;
    server.push(latest("v0.5.2"));
    let install = Install::new();
    let env = Env::empty();

    let (result, out) = updated(options(
        &server,
        &install.path,
        UpdateCommand::default(),
        &env,
    ))
    .await;

    assert_eq!(result.unwrap(), 0);
    assert!(
        out.contains("newer than the latest release (0.5.2)"),
        "{out}"
    );
    install.assert_untouched();
}

#[tokio::test]
async fn check_says_what_is_available_and_installs_nothing() {
    let server = ScriptedServer::start().await;
    server.push(latest("v0.7.0"));
    let install = Install::new();
    let env = Env::empty();

    let (result, out) = updated(options(&server, &install.path, check(), &env)).await;

    assert_eq!(result.unwrap(), 0);
    assert!(out.contains("0.7.0 is available (running 0.6.0)"), "{out}");
    assert_eq!(paths(&server), ["/repo/releases/latest"]);
    install.assert_untouched();
}

#[tokio::test]
async fn check_on_an_up_to_date_install_says_so() {
    let server = ScriptedServer::start().await;
    server.push(latest("v0.6.0"));
    let install = Install::new();
    let env = Env::empty();

    let (result, out) = updated(options(&server, &install.path, check(), &env)).await;

    assert_eq!(result.unwrap(), 0);
    assert!(out.contains("up to date"), "{out}");
}

#[tokio::test]
async fn a_named_tag_installs_without_asking_which_is_latest_even_when_older() {
    // `0.5.2` without its `v`: the tag is spelled the way the release names it.
    let server = ScriptedServer::start().await;
    let tarball = release(NEW);
    server.push(text(sums_for(&tarball))).push(text(tarball));
    let install = Install::new();
    let env = Env::empty();
    let mut options = options(&server, &install.path, tagged("0.5.2"), &env);
    options.probe = |_| Ok("0.5.2".to_owned());

    let (result, out) = updated(options).await;

    assert_eq!(result.unwrap(), 0);
    assert_eq!(install.bytes(), NEW);
    assert_eq!(
        paths(&server),
        [
            "/download/v0.5.2/SHA256SUMS".to_owned(),
            format!("/download/v0.5.2/{ASSET}"),
        ]
    );
    assert!(out.contains("Updated darkwire 0.6.0 to 0.5.2."), "{out}");
}

#[tokio::test]
async fn accepts_the_binary_mode_spelling_of_a_checksum_line() {
    let server = ScriptedServer::start().await;
    let tarball = release(NEW);
    server
        .push(text(format!("{} *{ASSET}\n", digest(&tarball))))
        .push(text(tarball));
    let install = Install::new();
    let env = Env::empty();

    let (result, _) = updated(options(&server, &install.path, tagged("v0.7.0"), &env)).await;

    assert_eq!(result.unwrap(), 0);
    assert_eq!(install.bytes(), NEW);
}

#[tokio::test]
async fn a_checksum_mismatch_installs_nothing_and_says_both_hashes() {
    let server = ScriptedServer::start().await;
    let tarball = release(NEW);
    let wrong = format!("{}  {ASSET}\n", "a".repeat(64));
    server.push(text(wrong)).push(text(tarball.clone()));
    let install = Install::new();
    let env = Env::empty();

    let (result, _) = updated(options(&server, &install.path, tagged("v0.7.0"), &env)).await;

    let error = result.unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.contains(&"a".repeat(64)), "{}", error.message);
    assert!(
        error.message.contains(&digest(&tarball)),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("Nothing has been installed"),
        "{}",
        error.message
    );
    install.assert_untouched();
}

#[tokio::test]
async fn sums_that_do_not_name_this_platform_install_nothing() {
    let server = ScriptedServer::start().await;
    server.push(text(format!("{}  darkwire-other.tar.gz\n", "0".repeat(64))));
    let install = Install::new();
    let env = Env::empty();

    let (result, _) = updated(options(&server, &install.path, tagged("v0.7.0"), &env)).await;

    let error = result.unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
    assert!(
        error.message.contains("does not mention"),
        "{}",
        error.message
    );
    assert_eq!(server.calls().len(), 1, "the tarball was fetched anyway");
    install.assert_untouched();
}

#[tokio::test]
async fn a_tarball_with_no_binary_in_it_installs_nothing() {
    // Only `<dir>/darkwire` counts. A file of that name at any other depth is
    // not where a release puts it.
    let server = ScriptedServer::start().await;
    let tarball = tarball(&[
        (
            "darkwire-v0.7.0-x86_64-unknown-linux-gnu/README.md",
            b"readme",
        ),
        ("darkwire", NEW),
        ("darkwire-v0.7.0-x86_64-unknown-linux-gnu/bin/darkwire", NEW),
    ]);
    server.push(text(sums_for(&tarball))).push(text(tarball));
    let install = Install::new();
    let env = Env::empty();

    let (result, _) = updated(options(&server, &install.path, tagged("v0.7.0"), &env)).await;

    let error = result.unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
    assert!(
        error.message.contains("does not contain a darkwire binary"),
        "{}",
        error.message
    );
    install.assert_untouched();
}

#[tokio::test]
async fn a_binary_that_does_not_start_is_not_installed() {
    let server = ScriptedServer::start().await;
    let tarball = release(NEW);
    server.push(text(sums_for(&tarball))).push(text(tarball));
    let install = Install::new();
    let env = Env::empty();
    let mut options = options(&server, &install.path, tagged("v0.7.0"), &env);
    options.probe = |_| Err(io::Error::other("exec format error"));

    let (result, _) = updated(options).await;

    let error = result.unwrap_err();
    assert!(
        error.message.contains("did not start (exec format error)"),
        "{}",
        error.message
    );
    install.assert_untouched();
}

#[tokio::test]
async fn a_binary_that_names_another_version_is_not_installed() {
    let server = ScriptedServer::start().await;
    let tarball = release(NEW);
    server.push(text(sums_for(&tarball))).push(text(tarball));
    let install = Install::new();
    let env = Env::empty();
    let mut options = options(&server, &install.path, tagged("v0.7.0"), &env);
    options.probe = |_| Ok("0.6.9".to_owned());

    let (result, _) = updated(options).await;

    let error = result.unwrap_err();
    assert!(
        error.message.contains("says it is 0.6.9, not 0.7.0"),
        "{}",
        error.message
    );
    install.assert_untouched();
}

#[tokio::test]
async fn a_download_that_fails_names_the_url_and_installs_nothing() {
    let server = ScriptedServer::start().await;
    server.push(ScriptedResponse::empty(404));
    let install = Install::new();
    let env = Env::empty();

    let (result, _) = updated(options(&server, &install.path, tagged("v0.7.0"), &env)).await;

    let error = result.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(
        error.message.contains("/download/v0.7.0/SHA256SUMS"),
        "{}",
        error.message
    );
    assert!(error.message.contains("404"), "{}", error.message);
    install.assert_untouched();
}

#[tokio::test]
async fn a_server_that_is_not_there_says_why() {
    // Bound and dropped, so the port refuses.
    let port = ScriptedServer::start().await.port();
    let server = ScriptedServer::start().await;
    let install = Install::new();
    let env = Env::empty();
    let mut options = options(&server, &install.path, check(), &env);
    options.api_base = format!("http://127.0.0.1:{port}/repo");

    let (result, _) = updated(options).await;

    let error = result.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(error.retryable);
    assert!(
        error.message.contains("/repo/releases/latest"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn a_latest_release_that_is_not_json_is_a_network_failure() {
    let server = ScriptedServer::start().await;
    server.push(text("<html>rate limited</html>"));
    let install = Install::new();
    let env = Env::empty();

    let (result, _) = updated(options(&server, &install.path, check(), &env)).await;

    assert_eq!(result.unwrap_err().kind, ErrorKind::Network);
}

#[tokio::test]
async fn a_tag_that_is_not_a_version_is_refused_before_any_request() {
    let server = ScriptedServer::start().await;
    let install = Install::new();
    let env = Env::empty();

    let (result, _) = updated(options(&server, &install.path, tagged("latest"), &env)).await;

    let error = result.unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(
        error.message.contains("vlatest is not a release tag"),
        "{}",
        error.message
    );
    assert!(server.calls().is_empty());
    install.assert_untouched();
}

#[tokio::test]
async fn a_source_build_is_refused_before_any_request() {
    let server = ScriptedServer::start().await;
    let root = tempfile::tempdir().unwrap();
    let release_dir = root.path().join("target").join("release");
    std::fs::create_dir_all(&release_dir).unwrap();
    let path = release_dir.join("darkwire");
    std::fs::write(&path, OLD).unwrap();
    let env = Env::empty();

    let (result, _) = updated(options(&server, &path, UpdateCommand::default(), &env)).await;

    let error = result.unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.contains("source build"), "{}", error.message);
    assert!(server.calls().is_empty());

    // Asking is still fine: `--check` writes nothing.
    server.push(latest("v0.7.0"));
    let (result, out) = updated(options(&server, &path, check(), &env)).await;
    assert_eq!(result.unwrap(), 0);
    assert!(out.contains("0.7.0 is available"), "{out}");
}

#[tokio::test]
async fn a_platform_with_no_release_build_is_refused_before_any_request() {
    let server = ScriptedServer::start().await;
    let install = Install::new();
    let env = Env::empty();
    let mut options = options(&server, &install.path, UpdateCommand::default(), &env);
    options.os = "windows";

    let (result, _) = updated(options).await;

    let error = result.unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
    assert!(
        error.message.contains("windows x86_64"),
        "{}",
        error.message
    );
    assert!(server.calls().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn a_directory_it_cannot_write_to_is_refused_before_any_request() {
    use std::os::unix::fs::PermissionsExt;

    let server = ScriptedServer::start().await;
    let install = Install::new();
    let env = Env::empty();
    let dir = install.dir.path();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root writes through a read-only mode, so there is nothing to observe.
    if tempfile::tempfile_in(dir).is_ok() {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let (result, _) = updated(options(
        &server,
        &install.path,
        UpdateCommand::default(),
        &env,
    ))
    .await;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    let error = result.unwrap_err();
    assert_eq!(error.kind, ErrorKind::PermissionDenied);
    assert!(
        error.message.contains("is not writable"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("sudo darkwire update"),
        "{}",
        error.message
    );
    assert!(server.calls().is_empty());
    install.assert_untouched();
}

#[tokio::test]
async fn interrupting_the_download_installs_nothing() {
    let server = ScriptedServer::start().await;
    let install = Install::new();
    let env = Env::empty();
    let cancel = CancellationToken::new();
    cancel.cancel();

    let (result, _) = updated_with(
        options(&server, &install.path, tagged("v0.7.0"), &env),
        &cancel,
    )
    .await;

    assert_eq!(result.unwrap_err().kind, ErrorKind::Aborted);
    install.assert_untouched();
}

#[test]
fn release_targets_are_the_four_the_release_builds() {
    assert_eq!(
        release_target("macos", "aarch64"),
        Some("aarch64-apple-darwin")
    );
    assert_eq!(
        release_target("macos", "x86_64"),
        Some("x86_64-apple-darwin")
    );
    assert_eq!(
        release_target("linux", "x86_64"),
        Some("x86_64-unknown-linux-gnu")
    );
    assert_eq!(
        release_target("linux", "aarch64"),
        Some("aarch64-unknown-linux-gnu")
    );
    assert_eq!(release_target("linux", "arm"), None);
    assert_eq!(release_target("windows", "x86_64"), None);
}

#[test]
fn the_probe_reads_the_version_a_binary_prints() {
    let printed = probe_binary(Path::new(env!("CARGO_BIN_EXE_darkwire"))).unwrap();
    assert_eq!(printed, darkwire::VERSION);
}

#[test]
fn the_probe_fails_on_a_file_that_does_not_run() {
    let install = Install::new();
    assert!(probe_binary(&install.path).is_err());
}
