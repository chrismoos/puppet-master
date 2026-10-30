#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDOR="${ROOT}/crates/pm-ghostty/vendor"
REVISION="$(tr -d '[:space:]' < "${VENDOR}/VERSION")"
ZIG_VERSION="0.17.0"
SOURCE="${GHOSTTY_SOURCE_DIR:-}"
ZIG="${GHOSTTY_ZIG:-$(command -v zig || true)}"
TARGETS="${GHOSTTY_TARGETS:-aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu aarch64-apple-darwin x86_64-apple-darwin wasm32}"
BUILD="${GHOSTTY_BUILD_DIR:-${ROOT}/target/ghostty-runtime}"

fail() { printf '[ghostty-runtime] error: %s\n' "$*" >&2; exit 1; }
[[ -n "${SOURCE}" && -e "${SOURCE}/.git" ]] || fail "set GHOSTTY_SOURCE_DIR to a clean pinned Ghostty checkout"
[[ -x "${ZIG}" ]] || fail "install Zig ${ZIG_VERSION} or set GHOSTTY_ZIG"
[[ "$("${ZIG}" version)" == "${ZIG_VERSION}" ]] || fail "expected Zig ${ZIG_VERSION}"
[[ "$(git -C "${SOURCE}" rev-parse HEAD)" == "${REVISION}" ]] || fail "Ghostty source revision differs from vendor/VERSION"
[[ -z "$(git -C "${SOURCE}" status --porcelain)" ]] || fail "Ghostty source checkout must be clean"
mkdir -p "${BUILD}"
for target in ${TARGETS}; do
  case "${target}" in
    aarch64-unknown-linux-gnu) zig_target=aarch64-linux-gnu ;;
    x86_64-unknown-linux-gnu) zig_target=x86_64-linux-gnu ;;
    aarch64-apple-darwin) zig_target=aarch64-macos ;;
    x86_64-apple-darwin) zig_target=x86_64-macos ;;
    wasm32) zig_target=wasm32-freestanding ;;
    *) fail "unsupported target ${target}" ;;
  esac
  output="${BUILD}/${target}"
  (cd "${SOURCE}"; "${ZIG}" build --cache-dir "${BUILD}/zig-cache" \
    -Demit-lib-vt -Dsimd=false -Dvt-features=-kitty_graphics,-glyph_protocol \
    -Dtarget="${zig_target}" -Doptimize=ReleaseSmall -Dstrip=true \
    -Dlib-version-string="0.1.0-dev+${REVISION}" -p "${output}")
  if [[ "${target}" == wasm32 ]]; then
    cp "${output}/bin/ghostty-vt.wasm" "${ROOT}/web/src/ws/ghostty/vendor/ghostty-vt.wasm"
    "${ZIG}" build-exe "${ROOT}/web/src/ws/ghostty/ghostty-write-pty.zig" \
      -target wasm32-freestanding -O ReleaseSmall -fno-entry -rdynamic \
      -femit-bin="${ROOT}/web/src/ws/ghostty/vendor/ghostty-write-pty.wasm"
    cp "${VENDOR}/VERSION" "${ROOT}/web/src/ws/ghostty/vendor/VERSION"
  else
    mkdir -p "${VENDOR}/${target}"
    cp "${output}/lib/libghostty-vt.a" "${VENDOR}/${target}/libghostty-vt-static.a"
    "${ZIG}" cc -target "${zig_target}" -Os -fPIC -DGHOSTTY_STATIC \
      -I "${SOURCE}/include" -c "${ROOT}/crates/pm-ghostty/src/bridge.c" \
      -o "${output}/bridge.o"
    "${ZIG}" ar rcs "${VENDOR}/${target}/libpm-ghostty-bridge.a" "${output}/bridge.o"
  fi
done
python3 - "${ROOT}" <<'PY'
import hashlib, pathlib, sys
root = pathlib.Path(sys.argv[1])
crate = root / 'crates/pm-ghostty'
paths = [crate / 'src/bridge.c', *sorted((crate / 'vendor').glob('*/*.a'))]
(crate / 'vendor/SHA256SUMS').write_text(''.join(
    f'{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(crate)}\n'
    for path in paths))
PY
printf '[ghostty-runtime] rebuilt %s at %s\n' "${TARGETS}" "${REVISION}"
