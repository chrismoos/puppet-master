#!/usr/bin/env bash
# Cross-language HPKE interop test: Rust hpke crate ↔ CryptoKit.
#
# Requires: Rust toolchain, Swift toolchain (Xcode or swift.org).
# macOS only — CryptoKit HPKE needs macOS 14+ / iOS 17+.
#
# This is NOT part of the normal test lane. Run it manually when
# changing the HPKE ciphersuite, the info parameter, or upgrading
# either the hpke crate or the Xcode toolchain.

set -euo pipefail
cd "$(dirname "$0")"

RUST_BIN=./target/release/hpke-interop-test
PASS=0
FAIL=0

step() { echo ""; echo "=== $1 ==="; }
pass() { echo "PASS  $1"; PASS=$((PASS + 1)); }
fail() { echo "FAIL  $1"; FAIL=$((FAIL + 1)); }

# ── Build ───────────────────────────────────────────────────────────

step "Building Rust interop binary"
cargo build --release --quiet

step "Swift self-test (CryptoKit-only, includes negative cases)"
swift hpke_interop.swift selftest

# ── Cross-language: Rust seals, Swift opens (production info) ──────

step "Rust seals with pm-push-hpke-v1 info, Swift opens"
RUST_OUT=$($RUST_BIN generate)
SK=$(echo "$RUST_OUT" | grep '^SK=' | cut -d= -f2)
ENC=$(echo "$RUST_OUT" | grep '^ENC=' | cut -d= -f2)
CT=$(echo "$RUST_OUT" | grep '^CT=' | cut -d= -f2)
SWIFT_OUT=$(swift hpke_interop.swift open "$SK" "$ENC" "$CT")
if echo "$SWIFT_OUT" | grep -q '^OK='; then
    pass "Rust→Swift with production info"
else
    fail "Rust→Swift with production info: $SWIFT_OUT"
fi

# ── Cross-language: Swift seals, Rust opens (production info) ──────

step "Swift seals with pm-push-hpke-v1 info, Rust opens"
SWIFT_OUT=$(swift hpke_interop.swift generate)
SK=$(echo "$SWIFT_OUT" | grep '^SK=' | cut -d= -f2)
ENC=$(echo "$SWIFT_OUT" | grep '^ENC=' | cut -d= -f2)
CT=$(echo "$SWIFT_OUT" | grep '^CT=' | cut -d= -f2)
RUST_OUT=$($RUST_BIN open "$SK" "$ENC" "$CT")
if echo "$RUST_OUT" | grep -q '^OK='; then
    pass "Swift→Rust with production info"
else
    fail "Swift→Rust with production info: $RUST_OUT"
fi

# ── Cross-keypair: Swift key, Rust seals, Swift opens ──────────────

step "Swift generates keypair, Rust seals to it, Swift opens"
SWIFT_GEN=$(swift hpke_interop.swift generate)
SK=$(echo "$SWIFT_GEN" | grep '^SK=' | cut -d= -f2)
PK=$(echo "$SWIFT_GEN" | grep '^PK=' | cut -d= -f2)
PT_HEX=$(printf '%s' '{"title":"test","counter":42}' | xxd -p | tr -d '\n')
RUST_SEALED=$($RUST_BIN seal "$PK" "$PT_HEX")
ENC=$(echo "$RUST_SEALED" | grep '^ENC=' | cut -d= -f2)
CT=$(echo "$RUST_SEALED" | grep '^CT=' | cut -d= -f2)
SWIFT_OPEN=$(swift hpke_interop.swift open "$SK" "$ENC" "$CT")
if echo "$SWIFT_OPEN" | grep -q '^OK='; then
    pass "cross-keypair Rust→Swift"
else
    fail "cross-keypair Rust→Swift: $SWIFT_OPEN"
fi

# ── Negative: Rust seals with production info, Swift opens with empty info ──

step "Negative: Rust seals with info, Swift opens with empty info (must fail)"
RUST_OUT=$($RUST_BIN generate)
SK=$(echo "$RUST_OUT" | grep '^SK=' | cut -d= -f2)
ENC=$(echo "$RUST_OUT" | grep '^ENC=' | cut -d= -f2)
CT=$(echo "$RUST_OUT" | grep '^CT=' | cut -d= -f2)
EMPTY_INFO=$(printf '' | xxd -p)
if swift hpke_interop.swift open "$SK" "$ENC" "$CT" --info "" 2>/dev/null; then
    fail "empty-info open should have failed"
else
    pass "mismatched info correctly rejected cross-language"
fi

# ── Summary ─────────────────────────────────────────────────────────

echo ""
echo "Cross-language: $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then exit 1; fi
