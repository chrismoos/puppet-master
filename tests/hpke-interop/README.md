# HPKE interop test

Cross-language interop proof for the HPKE ciphersuite used by push
notification sealing (#226). Verifies that the Rust `hpke` crate and
Apple CryptoKit produce byte-compatible ciphertext for:

    DHKEM(X25519, HKDF-SHA256) + HKDF-SHA256 + AES-128-GCM
    RFC 9180 Base mode

## What it proves

- **Both directions**: Rust seals → Swift opens, Swift seals → Rust opens.
- **Cross-keypair**: one side generates the key, the other seals to it.
- **Domain separation is bound**: sealing with `pm-push-hpke-v1` as
  info and opening with a different info or empty info fails with an
  authentication error. This confirms info feeds into the key schedule
  on both sides rather than being silently accepted.
- **Wrong-key rejection**: decryption with a different private key fails.
- **Notification JSON round-trip**: a realistic `SealedNotification`
  JSON is sealed and decrypted through the same wire format
  (`enc (32) || ciphertext || tag (16)`) the NSE uses.

## Requirements

- macOS 14+ (CryptoKit HPKE requires macOS 14 / iOS 17)
- Xcode or a Swift toolchain (`swift` on PATH)
- Rust toolchain (`cargo` on PATH)

This is **not** part of the normal test lane (`make test`, `make check`,
or `cargo nextest`). It produces standalone binaries and runs them
directly. Run it when:

- Changing the HPKE ciphersuite or info parameter
- Upgrading the `hpke` crate
- Upgrading Xcode or the Swift toolchain
- Modifying the NSE decryption code

## Running

```sh
cd tests/hpke-interop
./run.sh
```

The script builds the Rust binary (`cargo build --release`), runs the
Swift self-test (which includes the negative cases), then exercises
all four cross-language test paths. A non-zero exit means a failure.

Individual pieces can be run separately:

```sh
# Swift self-test only (no Rust needed)
swift hpke_interop.swift selftest

# Generate a Rust vector and open in Swift
cargo build --release
./target/release/hpke-interop-test generate
swift hpke_interop.swift open <sk> <enc> <ct>
```

## Files

| File | Purpose |
|------|---------|
| `hpke_interop.swift` | CryptoKit HPKE CLI: generate, seal, open, selftest |
| `src/main.rs` | Rust `hpke` crate CLI: generate, seal, open |
| `Cargo.toml` | Standalone workspace for the Rust binary |
| `run.sh` | Orchestrates all cross-language tests |
