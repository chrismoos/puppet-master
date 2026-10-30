#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
GHOSTTY_TARGETS=wasm32 exec "${REPO_DIR}/scripts/build-ghostty-runtime.sh"
