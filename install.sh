#!/bin/sh
# Install GhostAI: one binary, from the GitHub release, checksum-verified.
#
#   curl -fsSL https://raw.githubusercontent.com/therezor/GhostAI/main/install.sh | sh
#
# The whole reason this exists rather than a `curl` line in the README is the
# verification. The manual instructions ask a reader to download SHA256SUMS and
# run `shasum -c` themselves, and almost nobody does; here it is not a step that
# can be skipped, because the extract does not happen until the hash matches.
#
# Options, which reach a piped run as `| sh -s -- --version v1.2.3`:
#
#   --version <tag>   install this release instead of the latest
#   --dir <path>      install here instead of /usr/local/bin
#   --help
#
# GHOSTAI_INSTALL_DIR does the same as --dir, for a caller that would rather set
# an environment variable than pass an argument.

set -eu

REPO='therezor/GhostAI'
VERSION='latest'
INSTALL_DIR="${GHOSTAI_INSTALL_DIR:-/usr/local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
	cat <<'USAGE'
Install GhostAI.

Usage: install.sh [--version <tag>] [--dir <path>]

  --version <tag>   a release tag such as v1.2.3 (default: the latest release)
  --dir <path>      where to put the binary (default: /usr/local/bin, or
                    $GHOSTAI_INSTALL_DIR)
USAGE
}

while [ $# -gt 0 ]; do
	case "$1" in
		--version)
			[ $# -ge 2 ] || die '--version needs a tag, such as v1.2.3.'
			VERSION="$2"
			shift 2
			;;
		--dir)
			[ $# -ge 2 ] || die '--dir needs a path.'
			INSTALL_DIR="$2"
			shift 2
			;;
		-h | --help)
			usage
			exit 0
			;;
		*) die "unknown option: $1. Run with --help for the list." ;;
	esac
done

# The four targets the release builds. Anything else is a build from source, and
# saying so is more use than a 404 from the download.
os=$(uname -s)
arch=$(uname -m)
case "$os/$arch" in
	Darwin/arm64) target='aarch64-apple-darwin' ;;
	Darwin/x86_64) target='x86_64-apple-darwin' ;;
	Linux/x86_64 | Linux/amd64) target='x86_64-unknown-linux-gnu' ;;
	Linux/aarch64 | Linux/arm64) target='aarch64-unknown-linux-gnu' ;;
	*)
		die "no build for $os $arch. The release carries macOS and Linux on x86_64 and
arm64; everything else builds from source — see
https://github.com/$REPO/blob/main/docs/getting-started.md"
		;;
esac

# One of curl or wget, and one of the two spellings of sha256. A machine with
# neither half is rare enough to be worth one clear sentence.
if command -v curl >/dev/null 2>&1; then
	fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
	fetch() { wget -qO "$2" "$1"; }
else
	die 'neither curl nor wget is installed, and one of them has to be.'
fi

if command -v sha256sum >/dev/null 2>&1; then
	sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
	sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
	die 'neither sha256sum nor shasum is installed, so the download cannot be verified.'
fi

if [ "$VERSION" = 'latest' ]; then
	base="https://github.com/$REPO/releases/latest/download"
else
	base="https://github.com/$REPO/releases/download/$VERSION"
fi
asset="ghostai-$target.tar.gz"

work=$(mktemp -d)
cleanup() { rm -rf "$work"; }
trap cleanup EXIT INT TERM

say "Downloading $asset ($VERSION)…"
# curl and wget both say why on stderr, so these add the thing they cannot
# know: which URL was being asked for, and what a reader can do about it.
fetch "$base/$asset" "$work/$asset" ||
	die "could not download $base/$asset.
A 404 here means that release has no build for $target; anything else is the network."
fetch "$base/SHA256SUMS" "$work/SHA256SUMS" ||
	die "could not download $base/SHA256SUMS, so the download cannot be verified."

# Compared by hand rather than through `sha256sum -c`, because the flag that
# limits it to one file out of a release's five is spelled differently by
# coreutils and by shasum, and getting that wrong silently checks nothing.
expected=$(awk -v name="$asset" '$2 == name || $2 == "*" name { print $1; exit }' "$work/SHA256SUMS")
[ -n "$expected" ] || die "SHA256SUMS does not mention $asset."
actual=$(sha256 "$work/$asset")
if [ "$expected" != "$actual" ]; then
	die "checksum mismatch for $asset.
  expected $expected
  got      $actual
Nothing has been installed."
fi
say 'Checksum verified.'

tar -xzf "$work/$asset" -C "$work"
binary=$(find "$work" -type f -name ghostai -perm -u+x | head -n 1)
[ -n "$binary" ] || die "the tarball did not contain a ghostai binary."

# `sudo` only where it is actually needed, and only after saying so: a script
# read from the internet that reaches for root without a word is one nobody
# should run.
if [ -w "$INSTALL_DIR" ] || { [ ! -d "$INSTALL_DIR" ] && mkdir -p "$INSTALL_DIR" 2>/dev/null; }; then
	install -m 755 "$binary" "$INSTALL_DIR/ghostai"
elif command -v sudo >/dev/null 2>&1; then
	say "$INSTALL_DIR is not writable; using sudo to install there."
	sudo install -d -m 755 "$INSTALL_DIR"
	sudo install -m 755 "$binary" "$INSTALL_DIR/ghostai"
else
	die "$INSTALL_DIR is not writable and sudo is not installed.
Pass --dir with somewhere you can write, such as --dir \"\$HOME/.local/bin\"."
fi

say "Installed $INSTALL_DIR/ghostai"

# Two ways the install can be right and still not be what runs. Neither is worth
# refusing over — the binary is where it was asked to go — but both are worth a
# sentence, because the alternative is someone debugging a version they did not
# install.
found=$(command -v ghostai 2>/dev/null || true)
if [ -z "$found" ]; then
	say ''
	say "Note: $INSTALL_DIR is not on your PATH. Add it, or run the binary by its full path."
elif [ "$found" != "$INSTALL_DIR/ghostai" ]; then
	say ''
	say "Note: another ghostai comes first on your PATH, at $found."
	say "That one is what the shell will run until it is removed or the PATH order changes."
fi

say ''
"$INSTALL_DIR/ghostai" --version
say ''
say 'Next: ghostai serve — it prints a URL and a one-time code.'
