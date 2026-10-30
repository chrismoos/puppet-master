# Web terminal runtime

Session and workspace terminals use the same pinned Ghostty engine on the worker,
controller, and browser. Native builds maintain retained terminal state, and the
browser runs the WebAssembly build through the official C ABI and draws through
Canvas 2D. Live output remains a PTY byte stream. Attach, reconnect, and resize
transfer a binary Ghostty snapshot before live output resumes.

Snapshot compatibility identifies the Ghostty revision and shared build profile,
independently of the Puppet Master release. Browser connections check this identity
before attaching. A stale browser reports that the page needs reloading. Workers
without matching snapshot support remain registered, but terminal attachment
reports that the worker needs updating. CLI and mobile viewers receive VT output
from Ghostty's formatter instead of binary state.

Snapshots stay transient and never enter the retained terminal-output transcript.
A replay restores both screens, cursor, modes, drawing styles, and unfinished parser
input. Each snapshot currently carries the full retained history and is validated
before it replaces the browser terminal. Corrupt, truncated, or trailing data fails
restoration. The transport preserves output ordering across the snapshot boundary.

Keyboard input, paste, IME composition, mouse reporting, selection, scrollback, theme changes, HTTP and Puppet Master links, OSC 52 clipboard writes, and OSC 777 foreground command reports pass through the browser adapter. The browser terminal retains up to 32 MiB of scrollback, measured in the byte units used by the pinned Ghostty runtime. Clipboard writes from replayed snapshots are ignored so reconnecting cannot copy stale text.

If the WebAssembly runtime cannot initialize, session terminals show their existing failure state and workspace terminals report an unavailable renderer. No alternate terminal parser is selected.

The vendored WebAssembly files match `web/src/ws/ghostty/vendor/VERSION`. To rebuild them, provide a Ghostty source checkout at that exact commit and Zig 0.17.0:

```sh
GHOSTTY_SOURCE_DIR=/path/to/ghostty-source GHOSTTY_ZIG=/path/to/zig bash scripts/build-ghostty-runtime.sh
```

The browser loads one shared WebAssembly runtime for all terminal surfaces. A surface is created asynchronously; output, replay mode, and resize requests remain ordered while it initializes.

Native runtime artifacts and their shared build profile are documented in
`crates/pm-ghostty/vendor/README.md`. The runtime tests restore a fixture produced by
the native engine into the actual WebAssembly engine and continue partial input.
