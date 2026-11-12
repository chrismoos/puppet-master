# Pinned terminal runtime

These static libraries and the browser WebAssembly runtime use the Ghostty
revision in `VERSION`. Cargo links the checked-in archive for its target and
does not require Zig or a Ghostty checkout. The build verifies archive hashes
and the C bridge source hash against `SHA256SUMS` before linking.

Supported native targets are Linux on ARM64 and x86-64 and macOS on ARM64 and
x86-64. The browser uses wasm32-freestanding. All builds disable SIMD helper
libraries, Kitty graphics, and custom glyph registration. Other terminal
features, including snapshots, are enabled. The compatibility profile `pm-v1`
uses `-Dsimd=false -Dvt-features=-kitty_graphics,-glyph_protocol` with
`-Doptimize=ReleaseSmall -Dstrip=true`. Terminal behavior and Unicode width
policy use the defaults from that pinned source revision. Runtime callbacks remain local
to each terminal and must be installed after restoring a snapshot.

To rebuild every artifact, use Zig 0.17.0 and a clean Ghostty checkout at the
recorded revision:

```sh
GHOSTTY_SOURCE_DIR=/path/to/ghostty GHOSTTY_ZIG=/path/to/zig \
  bash scripts/build-ghostty-runtime.sh
```

The script checks the source revision and toolchain before replacing artifacts.
`GHOSTTY_TARGETS` selects a space-separated subset of the native target triples
and `wasm32`. Build intermediates default to `target/ghostty-runtime` and can be
moved with `GHOSTTY_BUILD_DIR`. An unsupported Cargo target fails explicitly.

A bridge source change requires rebuilding every native archive. Updating the
Ghostty revision requires rebuilding native and WebAssembly artifacts together.

Workers send terminal snapshots that browsers restore, so both must read the
same snapshot byte format. `crates/pm-protocol/ghostty-snapshot-compatibility`
names that format, and the browser and every worker report it. A controller
refuses snapshots from a worker reporting a different value, and asks a page
loaded with a different value to reload. It names the Ghostty revision whose
format the fleet last adopted, so it can be older than `VERSION`.

Change it only when the format changes. Before moving `VERSION`, regenerate
`testdata/hyperlink-history.snapshot` with the current library if the fixture
does not already cover what upstream changed:

```sh
cargo test -p pm-ghostty --release -- --ignored regenerate_hyperlink_history_fixture
```

Then, with the new library, the `pm-ghostty` tests and the web
`src/ws/ghostty/core.test.ts` restore tests must decode the snapshots written
by the old library, and restoring and encoding them again must give the same
bytes. Also restore a snapshot written by the new library with the previous
WebAssembly build, which an open page may still be running. If any of this
fails, set the identity to the new revision. Remote workers then need updating
before their terminals can be restored.
