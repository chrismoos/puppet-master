#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WEB_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_DIR="$(cd "${WEB_DIR}/.." && pwd)"
PINNED_REVISION="$(tr -d '[:space:]' < "${REPO_DIR}/web/src/ws/ghostty/vendor/VERSION")"
GHOSTTY_ZIG_VERSION="0.15.2"
GHOSTTY_SOURCE_DIR="${GHOSTTY_SOURCE_DIR:-}"
GHOSTTY_ZIG="${GHOSTTY_ZIG:-$(command -v zig || true)}"
VENDOR_DIR="${WEB_DIR}/src/ws/ghostty/vendor"

fail() {
  printf '[libghostty-vt-wasm] error: %s\n' "$*" >&2
  exit 1
}

[[ -n "${GHOSTTY_SOURCE_DIR}" && -e "${GHOSTTY_SOURCE_DIR}/.git" ]] || \
  fail "set GHOSTTY_SOURCE_DIR to the pinned Ghostty source checkout"
[[ -x "${GHOSTTY_ZIG}" ]] || fail "install Zig ${GHOSTTY_ZIG_VERSION} or set GHOSTTY_ZIG"
[[ "$("${GHOSTTY_ZIG}" version)" == "${GHOSTTY_ZIG_VERSION}" ]] || \
  fail "expected Zig ${GHOSTTY_ZIG_VERSION}"
[[ "$(git -C "${GHOSTTY_SOURCE_DIR}" rev-parse HEAD)" == "${PINNED_REVISION}" ]] || \
  fail "Ghostty source does not match web/src/ws/ghostty/vendor/VERSION"
[[ -z "$(git -C "${GHOSTTY_SOURCE_DIR}" status --porcelain)" ]] || \
  fail "Ghostty source checkout must be clean"

BUILD_DIR="$(mktemp -d)"
trap 'rm -rf "${BUILD_DIR}"' EXIT

(
  cd "${GHOSTTY_SOURCE_DIR}"
  "${GHOSTTY_ZIG}" build \
    -Demit-lib-vt \
    -Dtarget=wasm32-freestanding \
    -Doptimize=ReleaseSmall \
    -Dstrip=true \
    -Dlib-version-string="0.1.0-dev+${PINNED_REVISION}" \
    -p "${BUILD_DIR}"
)

mkdir -p "${VENDOR_DIR}"
cp "${BUILD_DIR}/bin/ghostty-vt.wasm" "${VENDOR_DIR}/ghostty-vt.wasm"
"${GHOSTTY_ZIG}" build-exe \
  "${WEB_DIR}/src/ws/ghostty/ghostty-write-pty.zig" \
  -target wasm32-freestanding \
  -O ReleaseSmall \
  -fno-entry \
  -rdynamic \
  -femit-bin="${VENDOR_DIR}/ghostty-write-pty.wasm"
chmod 0644 "${VENDOR_DIR}"/*.wasm
printf '[libghostty-vt-wasm] wrote artifacts for %s\n' "${PINNED_REVISION}"
