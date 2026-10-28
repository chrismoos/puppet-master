#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TEMP_ROOT="$(mktemp -d)"
trap 'rm -rf "${TEMP_ROOT}"' EXIT
FIXTURE_REPO="${TEMP_ROOT}/project with spaces"
SOURCE_DIR="${TEMP_ROOT}/Ghostty worktree"
BIN_DIR="${TEMP_ROOT}/bin"
PINNED_REVISION="$(tr -d '[:space:]' < "${REPO_DIR}/web/src/ws/ghostty/vendor/VERSION")"

mkdir -p "${FIXTURE_REPO}/web/scripts" "${FIXTURE_REPO}/web/src/ws/ghostty" \
  "${FIXTURE_REPO}/web/src/ws/ghostty/vendor" "${SOURCE_DIR}" "${BIN_DIR}"
cp "${SCRIPT_DIR}/build-libghostty-wasm.sh" "${FIXTURE_REPO}/web/scripts/"
cp "${REPO_DIR}/web/src/ws/ghostty/ghostty-write-pty.zig" \
  "${FIXTURE_REPO}/web/src/ws/ghostty/"
cp "${REPO_DIR}/web/src/ws/ghostty/vendor/VERSION" "${FIXTURE_REPO}/web/src/ws/ghostty/vendor/"
printf 'gitdir: %s\n' "${TEMP_ROOT}/gitdir" > "${SOURCE_DIR}/.git"

cat > "${BIN_DIR}/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == *"rev-parse HEAD"* ]]; then
  printf '%s\n' "${FAKE_GIT_REVISION:?}"
elif [[ "$*" == *"status --porcelain"* ]]; then
  printf '%s' "${FAKE_GIT_STATUS:-}"
else
  exit 2
fi
EOF

cat > "${BIN_DIR}/zig" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == version ]]; then
  printf '%s\n' "${FAKE_ZIG_VERSION:-0.15.2}"
elif [[ "${1:-}" == build ]]; then
  while (($#)); do
    if [[ "$1" == -p ]]; then
      mkdir -p "$2/bin"
      printf 'ghostty wasm' > "$2/bin/ghostty-vt.wasm"
      exit 0
    fi
    shift
  done
elif [[ "${1:-}" == build-exe ]]; then
  for argument in "$@"; do
    if [[ "$argument" == -femit-bin=* ]]; then
      output="${argument#-femit-bin=}"
      mkdir -p "$(dirname "$output")"
      printf 'pty wasm' > "$output"
      exit 0
    fi
  done
fi
exit 2
EOF
chmod +x "${BIN_DIR}/git" "${BIN_DIR}/zig"

PATH="${BIN_DIR}:${PATH}" GHOSTTY_SOURCE_DIR="${SOURCE_DIR}" \
  GHOSTTY_ZIG="${BIN_DIR}/zig" FAKE_GIT_REVISION="${PINNED_REVISION}" \
  "${FIXTURE_REPO}/web/scripts/build-libghostty-wasm.sh"
[[ -s "${FIXTURE_REPO}/web/src/ws/ghostty/vendor/ghostty-vt.wasm" ]]
[[ -s "${FIXTURE_REPO}/web/src/ws/ghostty/vendor/ghostty-write-pty.wasm" ]]

if PATH="${BIN_DIR}:${PATH}" GHOSTTY_SOURCE_DIR="${SOURCE_DIR}" \
  GHOSTTY_ZIG="${BIN_DIR}/zig" FAKE_GIT_REVISION="wrong-revision" \
  "${FIXTURE_REPO}/web/scripts/build-libghostty-wasm.sh" 2>/dev/null; then
  printf 'expected a mismatched source revision to fail\n' >&2
  exit 1
fi

if PATH="${BIN_DIR}:${PATH}" GHOSTTY_SOURCE_DIR="${SOURCE_DIR}" \
  GHOSTTY_ZIG="${BIN_DIR}/zig" FAKE_GIT_REVISION="${PINNED_REVISION}" \
  FAKE_GIT_STATUS=" M source.zig" \
  "${FIXTURE_REPO}/web/scripts/build-libghostty-wasm.sh" 2>/dev/null; then
  printf 'expected a dirty source checkout to fail\n' >&2
  exit 1
fi

if PATH="${BIN_DIR}:${PATH}" GHOSTTY_SOURCE_DIR="${SOURCE_DIR}" \
  GHOSTTY_ZIG="${BIN_DIR}/zig" FAKE_GIT_REVISION="${PINNED_REVISION}" \
  FAKE_ZIG_VERSION="0.14.1" \
  "${FIXTURE_REPO}/web/scripts/build-libghostty-wasm.sh" 2>/dev/null; then
  printf 'expected a mismatched Zig version to fail\n' >&2
  exit 1
fi
