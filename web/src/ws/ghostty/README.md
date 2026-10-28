# Web terminal runtime

Session and workspace terminals parse PTY output with the official `libghostty-vt` C ABI and draw through Canvas 2D. The existing terminal socket and daemon remain the source of PTY bytes, resize ownership, replay snapshots, and acknowledgments, so existing sessions and terminal state need no migration.

Keyboard input, paste, IME composition, mouse reporting, selection, scrollback, theme changes, HTTP and Puppet Master links, OSC 52 clipboard writes, and OSC 777 foreground command reports pass through the browser adapter. The browser terminal retains up to 32 MiB of scrollback, measured in the byte units used by the pinned Ghostty runtime. Clipboard writes from replayed snapshots are ignored so reconnecting cannot copy stale text.

If the WebAssembly runtime cannot initialize, session terminals show their existing failure state and workspace terminals report an unavailable renderer. No alternate terminal parser is selected.

The vendored WebAssembly files match `web/src/ws/ghostty/vendor/VERSION`. To rebuild them, provide a Ghostty source checkout at that exact commit and Zig 0.15.2:

```sh
GHOSTTY_SOURCE_DIR=/path/to/ghostty-source GHOSTTY_ZIG=/path/to/zig ./web/scripts/build-libghostty-wasm.sh
```

The browser loads one shared WebAssembly runtime for all terminal surfaces. A surface is created asynchronously; output, replay mode, and resize requests remain ordered while it initializes.
