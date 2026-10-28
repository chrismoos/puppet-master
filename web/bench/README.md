# Terminal browser probes

Run probes from the `web` directory:

- `pnpm test:layout` checks Canvas grid recovery, independent terminal state, scrollback, scrollbar dragging, and Shift+PageUp behavior in Chromium.
- `pnpm test:terminal-switch` records warm-layer creation, disposal, and switch timings for the Canvas terminal.
- `pnpm test:latency-baseline` measures protobuf decoding and write-drain costs with xterm as a development baseline. Its results do not measure the Canvas terminal.

The probes use the production terminal adapter and its public debug snapshots. Run `pnpm check` to typecheck the probe code with the web application.
