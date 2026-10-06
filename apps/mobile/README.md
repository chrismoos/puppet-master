# Puppet Master mobile

React Native companion app (Expo development builds, iOS 17 minimum).
Shared protocol/state/terminal logic comes from
`packages/client-core`; this package adds React Native adapters, the native
screens, and the bundled xterm.js WebView terminal.

Layout:

- `src/adapters/` — client-core platform adapters: WebSocket connector with
  bearer auth (legacy cookie fallback), secure-storage-backed key/value
  cache, app lifecycle binding.
- `src/auth/` — the device-auth client: enrollment against
  `POST /api/mobile/devices/enroll`, bearer access + rotating refresh token
  storage, and one-use socket ticket minting.
- `src/terminal/` — the RN/WebView message contract (`protocol.ts`), the
  touch-scroll gesture engine (`touchScroll.ts`) shared with the WebView, the
  native pan bridge (`panBridge.ts`), the pan scroll router
  (`scrollRouting.ts`), the touch mouse reporter (`mouseReports.ts`), the
  keyboard shift planner (`keyboardShift.ts`), the scrollbar and
  accessory-row visibility state machines (`scrollbarVisibility.ts`,
  `accessoryVisibility.ts`), base64 helpers, and the generated WebView
  bundle module (`gen/`, gitignored).
  WKWebView never delivers touchmove to page JS while native recognizers own
  the drag, so the terminal screen claims vertical pans with a `PanResponder`
  and forwards them over the message contract as `pan` events, and it also
  forwards every raw touch (claimed or not) as `touch` events. The WebView's
  router applies pans following xterm's wheel convention: the normal screen
  scrolls viewport scrollback with momentum and writes nothing to the PTY;
  the alternate screen with mouse tracking enabled receives one mouse wheel
  button report per line step at the touch cell (SGR or X10-style, matching
  the negotiated encoding) so mouse-tracking TUIs like Claude Code scroll
  their own buffer; the alternate screen with mouse tracking off receives
  line-quantized arrow-key sequences (application cursor mode respected).
  The touch mouse reporter turns the raw stream into the reports DOM events
  cannot provide: a tap becomes a press/release pair at the tapped cell (and
  focuses the terminal), a long-press or horizontal drag becomes a
  button-held selection drag with per-cell motion reports, and per-protocol
  delivery (x10/vt200/drag/any) is respected. From the first forwarded
  touch, the page stops WebKit's synthesized-from-touch mouse events so taps
  are never double-reported; wheel events from real pointing devices still
  reach xterm. Nothing filters the write path from xterm to the PTY socket:
  every byte the terminal emits, mouse reports included, reaches the socket
  (`terminal-web/src/writePath.ts`). xterm's own DOM scrollbar is hidden and
  an overlay scrollbar appears only during pan scrolling and momentum,
  fading shortly after (`scrollbarVisibility.ts`).
- `terminal-web/` — the WebView mini-app: xterm.js plus the client-core
  terminal socket/replay pipeline, bundled by `terminal-web/build.mjs` into a
  single self-contained HTML string. PTY bytes stay on the WebView's direct
  terminal WebSocket; only small JSON control/status messages cross the RN
  bridge.
- `src/screens/`, `App.tsx` — controller configuration, session
  list, terminal with a starter accessory row. The accessory row is hidden by
  default: it appears above the software keyboard while the keyboard is up,
  and the keyboard glyph in the header summons it without the keyboard or
  dismisses it while typing (`accessoryVisibility.ts`). Reclaimed space goes
  to the terminal, which refits on the resulting resize. When the software
  keyboard opens on iOS, the terminal container shifts with the keyboard
  animation itself: a translate transform tracks the keyboard's duration and
  curve so the bottom row moves immediately, and the real layout inset — and
  the single WebView refit and PTY resize it causes — applies only once the
  animation settles (`keyboardShift.ts`). The page holds refits between the
  shift's start and settle signals so no intermediate frame resizes the PTY,
  then sizes the terminal to the visual viewport (`keyboardViewport.ts`),
  reports the new PTY rows, and stays at the bottom of the scrollback if it
  was there, so the prompt row remains visible above the keyboard and
  restores when it hides. Android keeps the window-resize path.

Host-agnostic verification (part of `make check` / `make test`):

```sh
pnpm -F puppet-master-mobile run check
pnpm -F puppet-master-mobile run test
```

Build and run on macOS with Xcode using `make ios-run`, or build without
installing using `make ios-build`. Use `make ios-doctor` to check local build
requirements. These commands run from the repository root.

## iOS build numbers

From the repository root, `make ios-testflight` increments `expo.ios.buildNumber`
in `apps/mobile/app.json` before generating and archiving the iOS project.
For example, `43` becomes `44`. Git history does not affect the number.
`make ios-bump` increments that same saved value and commits it without building.
Running both commands increments the number twice.

Use `make ios-testflight BUILD_NUMBER=500` to set an explicit number instead.
Existing dotted numbers increment their final component (`1.2.9` becomes
`1.2.10`). A missing or invalid saved number fails rather than guessing.
A failed build leaves the incremented number in app.json for the next run.
Successful exports or uploads commit the number unless `--no-commit` is set.
`TESTFLIGHT_FLAGS="--dry-run"` leaves the saved number unchanged, and
`TESTFLIGHT_FLAGS="--from-archive"` reuses the existing archive's number.

TestFlight preflight rejects `PM_UI_TEST_BUILD=1` so a UI-test configuration
cannot be accidentally uploaded. Unset that variable before releasing.
Terminal fling and resize diagnostics require `__DEV__` to be `true`.
