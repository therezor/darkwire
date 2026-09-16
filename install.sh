#!/bin/sh
# Install DarkWire: one binary, from the GitHub release, checksum-verified.
#
#   curl -fsSL https://raw.githubusercontent.com/therezor/darkwire/main/install.sh | sh
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
# DARKWIRE_INSTALL_DIR does the same as --dir, for a caller that would rather set
# an environment variable than pass an argument.

set -eu

REPO='therezor/darkwire'
VERSION='latest'
INSTALL_DIR="${DARKWIRE_INSTALL_DIR:-/usr/local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
	cat <<'USAGE'
Install DarkWire.

Usage: install.sh [--version <tag>] [--dir <path>]

  --version <tag>   a release tag such as v1.2.3 (default: the latest release)
  --dir <path>      where to put the binary (default: /usr/local/bin, or
                    $DARKWIRE_INSTALL_DIR)
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

# `uname -m` names the kernel, and on Linux the userland need not agree with it.
# Raspberry Pi OS is the case that matters: the 32-bit image boots a 64-bit
# kernel on any Pi that can run one, so `uname -m` answers aarch64 on a machine
# whose every library is armhf. Trusting it there installs the aarch64 build,
# checksum and all, onto a system that cannot start it — the interpreter the
# binary names, /lib/ld-linux-aarch64.so.1, is not present, and the kernel's
# ENOENT reaches the shell as `not found` about a file that is plainly there.
# So ask the package manager what the userland is, and where there is no package
# manager take the width of a long, which is the userland's own.
if [ "$os" = Linux ]; then
	# `dpkg --print-architecture` answers for the userland, which is the fact
	# `uname -m` was standing in for, so where there is an answer it settles this.
	userland=''
	if command -v dpkg >/dev/null 2>&1; then
		case "$(dpkg --print-architecture 2>/dev/null)" in
			amd64) userland='x86_64' ;;
			arm64) userland='aarch64' ;;
			armhf | armel) userland='armhf' ;;
			i386) userland='i386' ;;
		esac
	fi
	if [ -n "$userland" ]; then
		arch="$userland"
	elif [ "$(getconf LONG_BIT 2>/dev/null)" = 32 ]; then
		# Only where dpkg said nothing, or said something this does not know: the
		# width of a long is the userland's own, so a 64-bit name over it is the
		# same lie. This never overrules dpkg — a 64-bit Pi OS answers arm64 here,
		# and demoting that on a second opinion is how a supported machine gets
		# turned away.
		case "$arch" in
			aarch64 | arm64) arch='armhf' ;;
			x86_64 | amd64) arch='i386' ;;
		esac
	fi
fi

case "$os/$arch" in
	Darwin/arm64) target='aarch64-apple-darwin' ;;
	Darwin/x86_64) target='x86_64-apple-darwin' ;;
	Linux/x86_64 | Linux/amd64) target='x86_64-unknown-linux-gnu' ;;
	Linux/aarch64 | Linux/arm64) target='aarch64-unknown-linux-gnu' ;;
	Linux/armhf | Linux/armv6l | Linux/armv7l | Linux/arm)
		die "no build for 32-bit ARM — every Linux build in the release is 64-bit.
On a Raspberry Pi this is worth reading twice: the 32-bit image boots a 64-bit kernel
on any Pi that can, so 'uname -m' here may well say aarch64 already. It is the
userland that cannot run the binary, and 'dpkg --print-architecture' is what says so.
Reflashing with the 64-bit Raspberry Pi OS image runs the aarch64 build; otherwise
build from source — see
https://github.com/$REPO/blob/main/docs/getting-started.md"
		;;
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
asset="darkwire-$target.tar.gz"

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
binary=$(find "$work" -type f -name darkwire -perm -u+x | head -n 1)
[ -n "$binary" ] || die "the tarball did not contain a darkwire binary."

# `sudo` only where it is actually needed, and only after saying so: a script
# read from the internet that reaches for root without a word is one nobody
# should run.
if [ -w "$INSTALL_DIR" ] || { [ ! -d "$INSTALL_DIR" ] && mkdir -p "$INSTALL_DIR" 2>/dev/null; }; then
	install -m 755 "$binary" "$INSTALL_DIR/darkwire"
elif command -v sudo >/dev/null 2>&1; then
	say "$INSTALL_DIR is not writable; using sudo to install there."
	sudo install -d -m 755 "$INSTALL_DIR"
	sudo install -m 755 "$binary" "$INSTALL_DIR/darkwire"
else
	die "$INSTALL_DIR is not writable and sudo is not installed.
Pass --dir with somewhere you can write, such as --dir \"\$HOME/.local/bin\"."
fi

say "Installed $INSTALL_DIR/darkwire"

# Two ways the install can be right and still not be what runs. Neither is worth
# refusing over — the binary is where it was asked to go — but both are worth a
# sentence, because the alternative is someone debugging a version they did not
# install.
found=$(command -v darkwire 2>/dev/null || true)
if [ -z "$found" ]; then
	say ''
	say "Note: $INSTALL_DIR is not on your PATH. Add it, or run the binary by its full path."
elif [ "$found" != "$INSTALL_DIR/darkwire" ]; then
	say ''
	say "Note: another darkwire comes first on your PATH, at $found."
	say "That one is what the shell will run until it is removed or the PATH order changes."
fi

say ''
"$INSTALL_DIR/darkwire" --version
say ''
say 'Next: darkwire serve — it prints a URL and a one-time code.'
