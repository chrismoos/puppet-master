#!/bin/sh
set -eu

missing_dependencies=false
for command_name in cc ld.lld make pkg-config curl; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        missing_dependencies=true
        break
    fi
done

run_as_root() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    else
        sudo -n "$@"
    fi
}

if [ "$missing_dependencies" = true ]; then
    if ! command -v apt-get >/dev/null 2>&1; then
        echo "Rust CI requires cc, lld, make, pkg-config, and curl." >&2
        exit 1
    fi
    certificate_bundle="${SSL_CERT_FILE:-${CURL_CA_BUNDLE:-/etc/ssl/certs/ca-certificates.crt}}"
    set -- build-essential lld pkg-config curl
    if [ ! -r "$certificate_bundle" ] || [ ! -s "$certificate_bundle" ]; then
        set -- "$@" ca-certificates
    fi
    run_as_root apt-get update
    run_as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-upgrade --no-install-recommends "$@"
fi

cargo_home="${CARGO_HOME:-${HOME:?HOME must be set}/.cargo}"
export PATH="$cargo_home/bin:$PATH"

if ! command -v rustup >/dev/null 2>&1; then
    installer=$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/pm-rustup.XXXXXX")
    trap 'rm -f "$installer"' 0 HUP INT TERM
    curl --proto '=https' --tlsv1.2 --fail --silent --show-error \
        https://sh.rustup.rs --output "$installer"
    sh "$installer" -y --no-modify-path --profile minimal --default-toolchain none
fi

rustup toolchain install stable --profile minimal --component rustfmt --component clippy
rustup default stable

if [ -n "${GITHUB_PATH:-}" ]; then
    printf '%s\n' "$cargo_home/bin" >> "$GITHUB_PATH"
fi
if [ -n "${GITHUB_ENV:-}" ]; then
    printf '%s\n' 'RUSTUP_TOOLCHAIN=stable' >> "$GITHUB_ENV"
fi
