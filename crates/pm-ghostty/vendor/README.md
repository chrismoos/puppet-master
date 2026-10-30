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
Ghostty revision requires rebuilding native and WebAssembly artifacts together
and reviewing the snapshot compatibility identity before deploying mixed hosts.
