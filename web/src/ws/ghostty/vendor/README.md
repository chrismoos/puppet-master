# Web terminal runtime

The web terminal ships the libghostty-vt WebAssembly binary and its PTY callback
trampoline under `web/src/ws/ghostty/vendor/`. Ordinary web builds use these
checked-in binaries and do not require Zig.

To rebuild them, use Zig 0.17.0 and a clean Ghostty source checkout at the commit
recorded in `VERSION`:

```sh
GHOSTTY_SOURCE_DIR=/path/to/ghostty GHOSTTY_ZIG=/path/to/zig \
  bash web/scripts/build-libghostty-wasm.sh
```

Run this command from the repository root. The script rejects a different source
revision, a dirty source checkout, or a different Zig version before replacing
either binary. Keep the two binaries and the native archives in `crates/pm-ghostty/vendor` together when updating the runtime.

Validate the build script's input checks with
`bash web/scripts/test-build-libghostty-wasm.sh`. The web unit suite exercises the
actual bundled WebAssembly runtime, including input encoding, scrollback,
selection, and ABI mappings. Browser terminal suites exercise rendering and the
daemon's replay and size-ownership contracts.
