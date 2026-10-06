#!/bin/sh
# Installs pm and, when given enrollment details, joins this machine to a
# controller as a worker.
#
#   curl -fsSL https://dl.puppet-master.xyz/install.sh | sh
#   curl -fsSL https://dl.puppet-master.xyz/install.sh | sh -s -- \
#     --controller wss://host:7676 --token <enrollment>
#
# This runs before any trusted pm binary exists, so it checks the
# published SHA-256 over TLS rather than a signature. Once installed, pm
# verifies its own updates against the release key compiled into it.
set -eu

BASE_URL="${PM_BASE_URL:-https://dl.puppet-master.xyz}"
INSTALL_DIR="${PM_INSTALL_DIR:-}"
VERSION=""
CHANNEL=""
CONTROLLER=""
TOKEN=""
MODIFY_PATH=1

usage() {
    cat <<'USAGE'
Installs pm, and joins this machine to a controller when given enrollment
details.

  --controller <url>    controller to join as a worker, needs --token
  --token <token>       enrollment token minted in Manage -> Hosts
  --version <version>   install this version instead of the newest
  --channel <name>      install the newest build on this release channel
  --dir <path>          install here instead of the default location
  --no-modify-path      do not touch shell startup files
USAGE
}

die() { printf 'install: %s\n' "$1" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required"; }

while [ $# -gt 0 ]; do
    case "$1" in
        --controller) CONTROLLER="${2:-}"; shift 2 ;;
        --token) TOKEN="${2:-}"; shift 2 ;;
        --version) VERSION="${2:-}"; shift 2 ;;
        --channel) CHANNEL="${2:-}"; shift 2 ;;
        --dir) INSTALL_DIR="${2:-}"; shift 2 ;;
        --no-modify-path) MODIFY_PATH=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option $1" ;;
    esac
done

need uname
need install
if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1"; }
    fetch_to() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO- "$1"; }
    fetch_to() { wget -qO "$2" "$1"; }
else
    die "curl or wget is required"
fi

case "$(uname -s)" in
    Linux) os=unknown-linux-gnu ;;
    Darwin) os=apple-darwin ;;
    *) die "unsupported operating system $(uname -s)" ;;
esac
case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    arm64|aarch64) arch=aarch64 ;;
    *) die "unsupported architecture $(uname -m)" ;;
esac
TARGET="$arch-$os"
[ "$TARGET" = "x86_64-apple-darwin" ] && die "no build is published for Intel macOS"

if [ -n "$VERSION" ] && [ -n "$CHANNEL" ]; then
    die "--version and --channel each choose what to install, pass only one"
fi
if [ -n "$VERSION" ]; then
    manifest_url="$BASE_URL/v$VERSION/release.json"
elif [ -n "$CHANNEL" ] && [ "$CHANNEL" != stable ]; then
    manifest_url="$BASE_URL/channels/$CHANNEL.json"
else
    manifest_url="$BASE_URL/latest.json"
fi
manifest=$(fetch "$manifest_url") || die "cannot read $manifest_url"

# Pull one string out of the manifest without assuming jq is installed.
field() {
    printf '%s' "$manifest" | tr -d '\n ' | sed -n "s/.*\"$1\":\"\\([^\"]*\\)\".*/\\1/p"
}
artifact_field() {
    printf '%s' "$manifest" | tr -d '\n ' |
        sed -n "s/.*\"$TARGET\":{\\([^}]*\\)}.*/\\1/p" |
        sed -n "s/.*\"$1\":\"\\([^\"]*\\)\".*/\\1/p"
}

version=$(field version)
[ -n "$version" ] || die "$manifest_url names no version"
path=$(artifact_field path)
sha=$(artifact_field sha256)
[ -n "$path" ] && [ -n "$sha" ] || die "release $version publishes no build for $TARGET"

if [ -z "$INSTALL_DIR" ]; then
    if [ -w /usr/local/bin ] 2>/dev/null; then
        INSTALL_DIR=/usr/local/bin
    else
        INSTALL_DIR="$HOME/.local/bin"
    fi
fi
mkdir -p "$INSTALL_DIR"

# Puts the install directory on PATH for future shells. Each line is
# self-guarding, so sourcing a file twice cannot stack duplicates, and
# the marker keeps a re-run from appending a second copy.
MARKER="# added by pm install.sh"

append_once() {
    file="$1"
    line="$2"
    [ -e "$file" ] || : > "$file" || return 1
    if grep -Fq "$MARKER" "$file" 2>/dev/null; then
        return 0
    fi
    printf '\n%s\n%s\n' "$MARKER" "$line" >> "$file" || return 1
    printf 'Added %s to PATH in %s\n' "$INSTALL_DIR" "$file" >&2
    CHANGED_A_PROFILE=1
}

add_to_path() {
    posix_line="case \":\$PATH:\" in *\":$INSTALL_DIR:\"*) ;; *) export PATH=\"$INSTALL_DIR:\$PATH\" ;; esac"
    CHANGED_A_PROFILE=0
    case "$(basename "${SHELL:-sh}")" in
        fish)
            mkdir -p "$HOME/.config/fish/conf.d" 2>/dev/null || true
            append_once "$HOME/.config/fish/conf.d/pm.fish" "fish_add_path $INSTALL_DIR"
            ;;
        zsh)
            append_once "${ZDOTDIR:-$HOME}/.zshrc" "$posix_line"
            ;;
        bash)
            append_once "$HOME/.bashrc" "$posix_line"
            # A login shell, which is what macOS terminals start, reads
            # this one instead.
            [ -e "$HOME/.bash_profile" ] && append_once "$HOME/.bash_profile" "$posix_line"
            ;;
        *)
            append_once "$HOME/.profile" "$posix_line"
            ;;
    esac
    if [ "$CHANGED_A_PROFILE" -eq 1 ]; then
        printf 'Open a new shell, or run: export PATH="%s:$PATH"\n' "$INSTALL_DIR" >&2
    else
        printf 'Add %s to your PATH to run pm.\n' "$INSTALL_DIR" >&2
    fi
}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/pm-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM

printf 'Downloading pm %s for %s\n' "$version" "$TARGET" >&2
fetch_to "$BASE_URL/$path" "$tmp/pm" || die "cannot download $BASE_URL/$path"

if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/pm" | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$tmp/pm" | cut -d' ' -f1)
else
    die "sha256sum or shasum is required to verify the download"
fi
[ "$actual" = "$sha" ] || die "downloaded pm has digest $actual, expected $sha"

install -m 0755 "$tmp/pm" "$INSTALL_DIR/pm"
printf 'Installed pm %s to %s/pm\n' "$version" "$INSTALL_DIR" >&2

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        if [ "$MODIFY_PATH" -eq 1 ]; then
            add_to_path
        else
            printf 'Add %s to your PATH to run pm.\n' "$INSTALL_DIR" >&2
        fi
        ;;
esac

if [ -n "$CONTROLLER" ]; then
    [ -n "$TOKEN" ] || die "--controller needs --token"
    printf 'Joining %s as a worker\n' "$CONTROLLER" >&2
    exec "$INSTALL_DIR/pm" worker --controller "$CONTROLLER" --token "$TOKEN"
fi
