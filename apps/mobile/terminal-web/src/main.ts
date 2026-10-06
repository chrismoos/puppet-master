import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";

import type { SocketLike } from "@puppet-master/client-core/platform";

import { TerminalGestureInput } from "../../src/terminal/gestureInput";
import {
  followingBottom,
  linesFromBottom,
  terminalHeightPx,
  viewportForLinesFromBottom,
  viewportRestoreDelta,
} from "../../src/terminal/keyboardViewport";
import { encodeViewMessage, parseHostMessage, type ViewMessage } from "../../src/terminal/protocol";
import {
  type MouseReportEncoding,
  type ScrollModeSnapshot,
  type TerminalCell,
} from "../../src/terminal/scrollRouting";
import { ScrollbarVisibility } from "../../src/terminal/scrollbarVisibility";
import { TerminalViewSession } from "./session";
import { computeFitDimensions } from "../../src/terminal/terminalFit";
import { connectTerminalOutput } from "./writePath";

declare global {
  interface Window {
    ReactNativeWebView?: { postMessage(message: string): void };
  }
}

const METRICS_INTERVAL_MS = 2_000;

function post(msg: ViewMessage): void {
  window.ReactNativeWebView?.postMessage(encodeViewMessage(msg));
}

const TERMINAL_FONT_SIZE = 13;

const term = new Terminal({
  scrollback: 5_000,
  fontSize: TERMINAL_FONT_SIZE,
  fontFamily: "Menlo, monospace",
  allowProposedApi: true,
  // The accessibility tree mirrors rendered rows as DOM text. On iOS this
  // is also the surface WebKit uses for its native loupe and selection menu.
  screenReaderMode: true,
});
const container = document.getElementById("terminal");
if (!container) throw new Error("terminal container missing");
term.open(container);

interface FitInternals {
  _core?: {
    _renderService?: {
      clear?: () => void;
      dimensions?: { css?: { cell?: { width?: number; height?: number } } };
    };
  };
}

/**
 * Sizes the terminal to the full container width and height without
 * reserving a scrollbar gutter. The mobile terminal hides xterm's
 * native scrollbar and uses a custom overlay indicator, so the stock
 * FitAddon's 14 px gutter subtraction gives the PTY a column count
 * that disagrees with the visible rendering — off by one on most
 * phone widths. This mirrors the web version's fitFullWidth().
 */
function measureTerminal() {
  const element = term.element;
  const host = element?.parentElement;
  if (!element || !host) return null;
  const core = (term as unknown as FitInternals)._core;
  const cell = core?._renderService?.dimensions?.css?.cell;
  const cellWidth = cell?.width ?? 0;
  const cellHeight = cell?.height ?? 0;

  const hostStyle = window.getComputedStyle(host);
  const elementStyle = window.getComputedStyle(element);
  const paddingHorizontal =
    parseFloat(elementStyle.getPropertyValue("padding-left")) +
    parseFloat(elementStyle.getPropertyValue("padding-right"));
  const paddingVertical =
    parseFloat(elementStyle.getPropertyValue("padding-top")) +
    parseFloat(elementStyle.getPropertyValue("padding-bottom"));
  const hostWidth = parseInt(hostStyle.getPropertyValue("width"), 10);
  const hostHeight = parseInt(hostStyle.getPropertyValue("height"), 10);

  return computeFitDimensions(
    hostWidth, hostHeight, paddingHorizontal, paddingVertical, cellWidth, cellHeight,
  );
}

function fitTerminal(): boolean {
  const dims = measureTerminal();
  if (!dims || (dims.cols === term.cols && dims.rows === term.rows)) return false;
  const core = (term as unknown as FitInternals)._core;
  core?._renderService?.clear?.();
  term.resize(dims.cols, dims.rows);
  return true;
}

// Renderer policy is decided from physical-device measurements later in the
// spike; try WebGL and fall back to the DOM renderer where it is unavailable.
let renderer: "webgl" | "dom" = "dom";
try {
  const webgl = new WebglAddon();
  term.loadAddon(webgl);
  renderer = "webgl";
  webgl.onContextLoss(() => {
    webgl.dispose();
    renderer = "dom";
  });
} catch {
  renderer = "dom";
}

const session = new TerminalViewSession({
  post,
  measureSize: measureTerminal,
  openSocket: (url, subprotocol) => new WebSocket(url, subprotocol) as unknown as SocketLike,
  term: {
    write: (data, callback) => term.write(data, callback),
    resize: (cols, rows) => term.resize(cols, rows),
    get cols() {
      return term.cols;
    },
    get rows() {
      return term.rows;
    },
  },
  now: () => Date.now(),
  captureViewport: () => linesFromBottom(term.buffer.active.viewportY, term.buffer.active.baseY),
  restoreViewport: (bookmark) => {
    const buffer = term.buffer.active;
    term.scrollToLine(viewportForLinesFromBottom(buffer.baseY, bookmark));
    updateFollowingState();
  },
  viewportRevision: () => userViewportRevision,
});

const encoder = new TextEncoder();

// xterm's public modes expose the mouse tracking mode but not the negotiated
// report encoding, so that one is read from the core mouse service. Modern
// TUIs request SGR, so it is the fallback if the internal shape moves, and
// pixel-position reports (SGR_PIXELS) are approximated with cell coordinates.
function mouseReportEncoding(): MouseReportEncoding {
  const internals = term as unknown as {
    _core?: { coreMouseService?: { activeEncoding?: string } };
  };
  return internals._core?.coreMouseService?.activeEncoding === "DEFAULT" ? "default" : "sgr";
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

// Cached layout measurements to avoid per-event getBoundingClientRect() calls.
// Invalidated on refit; read lazily on first scroll/touch after a refit.
let cachedScreen: HTMLElement | null = null;
let cachedRect: DOMRect | null = null;

function invalidateScreenCache(): void {
  cachedRect = null;
}

function screenRect(): { el: HTMLElement; rect: DOMRect } | null {
  if (!cachedScreen) cachedScreen = document.querySelector<HTMLElement>(".xterm-screen");
  if (!cachedScreen) return null;
  if (!cachedRect) cachedRect = cachedScreen.getBoundingClientRect();
  return { el: cachedScreen, rect: cachedRect };
}

function cellFromPoint(xPx: number, yPx: number): TerminalCell {
  const s = screenRect();
  if (!s || term.cols <= 0 || term.rows <= 0) return { col: 1, row: 1 };
  const { rect } = s;
  if (rect.width <= 0 || rect.height <= 0) return { col: 1, row: 1 };
  const col = Math.floor(((xPx - rect.left) / rect.width) * term.cols) + 1;
  const row = Math.floor(((yPx - rect.top) / rect.height) * term.rows) + 1;
  return { col: clamp(col, 1, term.cols), row: clamp(row, 1, term.rows) };
}

function modeSnapshot(): ScrollModeSnapshot {
  return {
    bufferType: term.buffer.active.type,
    applicationCursorKeys: term.modes.applicationCursorKeysMode,
    mouseTracking: term.modes.mouseTrackingMode,
    mouseEncoding: mouseReportEncoding(),
  };
}

function injectInput(data: string): void {
  session.sendInput(encoder.encode(data));
}

const input = new TerminalGestureInput(
  {
    scrollLines: (lines) => term.scrollLines(lines),
    input: injectInput,
    lineHeightPx: () => {
      const s = screenRect();
      return s && term.rows > 0 ? s.rect.height / term.rows : 0;
    },
    cellFromPoint,
    modes: modeSnapshot,
    // A follow-up tap belongs to WebKit while native text is selected: it
    // presents or dismisses the iOS edit menu. Focusing xterm's textarea at
    // that point steals the tap and leaves the selection UI stuck.
    focus: () => {
      if (!term.hasSelection()) term.focus();
    },
  },
  { onViewportScroll: scrollbarActivity, startFling: runFling },
);

// Make xterm's visible-row mirror an editable WebKit selection surface.
// `inputmode=none` keeps a selection gesture from opening the keyboard, while
// contenteditable makes iOS offer Paste in the same native menu as Copy and
// Select All. Pasted text is routed to the PTY and never mutates the mirror.
const nativeSelectionTree = term.element?.querySelector<HTMLElement>(".xterm-accessibility-tree");
if (nativeSelectionTree) {
  nativeSelectionTree.contentEditable = "plaintext-only";
  nativeSelectionTree.setAttribute("inputmode", "none");
  nativeSelectionTree.setAttribute("spellcheck", "false");
  nativeSelectionTree.addEventListener("paste", (event) => {
    event.preventDefault();
    event.stopPropagation();
    const text = event.clipboardData?.getData("text/plain") ?? "";
    if (!text) return;
    injectInput(bracketedPasteMode() ? `\x1b[200~${text}\x1b[201~` : text);
  });
  nativeSelectionTree.addEventListener("beforeinput", (event) => event.preventDefault());
}

connectTerminalOutput(term, session);
term.onTitleChange((title) => post({ type: "title", title }));
function bracketedPasteMode(): boolean {
  const internals = term as unknown as {
    _core?: { coreService?: { decPrivateModes?: { bracketedPasteMode?: boolean } } };
  };
  return internals._core?.coreService?.decPrivateModes?.bracketedPasteMode ?? false;
}

term.onSelectionChange(() => post({
  type: "selection",
  active: term.hasSelection(),
  bracketedPasteMode: bracketedPasteMode(),
}));

// Once the native host proves it forwards raw touches, the reporter is the
// authoritative source for touch-derived mouse reports, and the mouse events
// WebKit synthesizes from the same taps are stopped before xterm can report
// them a second time. Wheel events stay live: they only come from real
// pointing devices, and their reports must reach the PTY.
const SYNTHESIZED_MOUSE_EVENTS = ["mousedown", "mouseup", "mousemove", "click", "dblclick"] as const;
let hostOwnsMouse = false;

function claimMouseOwnership(): void {
  if (hostOwnsMouse) return;
  hostOwnsMouse = true;
  for (const type of SYNTHESIZED_MOUSE_EVENTS) {
    window.addEventListener(
      type,
      (ev) => {
        // The accessibility mirror is the native iOS selection surface.
        // WebKit needs its synthesized taps here to present and dismiss the
        // edit menu and to move/collapse the selection handles.
        if (ev.target instanceof Element && ev.target.closest(".xterm-accessibility")) return;
        ev.stopImmediatePropagation();
        ev.preventDefault();
      },
      { capture: true },
    );
  }
}

const scrollbarEl = document.getElementById("scrollbar");
const scrollbarVis = new ScrollbarVisibility();
const SCROLLBAR_MIN_THUMB_PX = 24;
let scrollbarLoopRunning = false;

// Track whether the viewport is following the bottom of the scrollback.
// The native shell owns the scroll-to-bottom control; the page reports
// following state changes so the shell can show/hide it.
let isFollowing = true;
let userViewportRevision = 0;

function updateFollowingState(): void {
  const buffer = term.buffer.active;
  const following = followingBottom(buffer.viewportY, buffer.baseY);
  if (following === isFollowing) return;
  isFollowing = following;
  post({ type: "following", following });
}

term.onScroll(updateFollowingState);

function syncScrollbar(nowMs: number): void {
  updateFollowingState();
  if (!scrollbarEl) return;
  if (!input.scrollActive()) scrollbarVis.settle(nowMs);
  const opacity = scrollbarVis.opacity(nowMs);
  const buffer = term.buffer.active;
  const s = screenRect();
  const trackPx = s ? s.rect.height : 0;
  if (opacity <= 0 || buffer.length <= term.rows || trackPx <= 0) {
    scrollbarEl.style.opacity = "0";
    return;
  }
  const thumbPx = Math.max(SCROLLBAR_MIN_THUMB_PX, (trackPx * term.rows) / buffer.length);
  const maxTopPx = trackPx - thumbPx;
  const maxViewportY = buffer.length - term.rows;
  const topPx = maxViewportY > 0 ? (buffer.viewportY / maxViewportY) * maxTopPx : 0;
  scrollbarEl.style.height = `${thumbPx}px`;
  scrollbarEl.style.transform = `translateY(${topPx}px)`;
  scrollbarEl.style.opacity = String(opacity);
}

function scrollbarLoop(nowMs: number): void {
  syncScrollbar(nowMs);
  if (scrollbarVis.phase(nowMs) === "hidden") {
    scrollbarLoopRunning = false;
    return;
  }
  requestAnimationFrame(scrollbarLoop);
}

function scrollbarActivity(): void {
  userViewportRevision += 1;
  scrollbarVis.activity();
  if (!scrollbarLoopRunning) {
    scrollbarLoopRunning = true;
    requestAnimationFrame(scrollbarLoop);
  }
}

let flingFrame = 0;

function runFling(): void {
  cancelAnimationFrame(flingFrame);
  const step = (nowMs: number): void => {
    input.flingStep(nowMs);
    if (input.flingActive()) flingFrame = requestAnimationFrame(step);
  };
  flingFrame = requestAnimationFrame(step);
}

function touchPoint(touch: Touch, timeMs: number) {
  return { x: touch.clientX, y: touch.clientY, timeMs };
}

// Under a React Native host the native layer claims plain drags and
// forwards them as pan host messages, because WKWebView's own recognizers
// consume drags before this document sees a touchmove. DOM touch scrolling
// stays on for plain-browser hosts only, so the two paths never both drive.
const hostDrivesPan = Boolean(window.ReactNativeWebView);

if (!hostDrivesPan) {
  container.addEventListener(
    "touchstart",
    (event) => {
      const touch = event.touches[0];
      if (touch) input.pan("start", touchPoint(touch, event.timeStamp), event.touches.length);
    },
    { passive: true },
  );
  container.addEventListener(
    "touchmove",
    (event) => {
      const touch = event.touches[0];
      if (!touch) return;
      input.pan("move", touchPoint(touch, event.timeStamp));
      if (input.scrolling()) event.preventDefault();
    },
    { passive: false },
  );
  container.addEventListener(
    "touchend",
    (event) => {
      const claimed = input.scrolling();
      const touch = event.changedTouches[0];
      input.pan(
        "end",
        touch ? touchPoint(touch, event.timeStamp) : { x: 0, y: 0, timeMs: event.timeStamp },
      );
      if (claimed) event.preventDefault();
    },
    { passive: false },
  );
  container.addEventListener(
    "touchcancel",
    (event) => input.pan("cancel", { x: 0, y: 0, timeMs: event.timeStamp }),
    { passive: true },
  );
}

// Coalesces resize signals into one trailing refit, keeping PTY SIGWINCHes
// rare. The native keyboard shift is a useNativeDriver transform that produces
// no page-side resize events, so the only refit comes from the single inset
// change at keyboard-event time.
const REFIT_DEBOUNCE_MS = 80;
let refitTimer: ReturnType<typeof setTimeout> | undefined;

function fitAndResize(): void {
  const prevCols0 = term.cols;
  const prevRows0 = term.rows;
  invalidateScreenCache();
  // When the native host drives layout (keyboard shift + insets), the WebView
  // frame IS the terminal size and the container's fixed-positioning tracks it
  // automatically. Setting the CSS height from window.innerHeight here races
  // the frame resize: the height update lands before the browser reflects the
  // new viewport, so fitAddon computes cols/rows from the stale size and the
  // PTY gets told the wrong geometry. Only standalone (non-RN) hosts, where
  // the visual viewport can shrink independently (e.g. iOS Safari keyboard),
  // still need the explicit height override.
  if (!hostDrivesPan) {
    const height = terminalHeightPx({
      layoutHeightPx: window.innerHeight,
      visualHeightPx: window.visualViewport?.height ?? null,
    });
    if (container && height > 0) container.style.height = `${height}px`;
  }

  // When the container has no layout — backgrounded app, mid-transition, or
  // before the first layout pass — fitting computes a degenerate size
  // (cols=2) from the zero-width parent. Fitting at that moment would send
  // the PTY a size the view does not have, causing the agent to reformat its
  // TUI at two columns; the damage persists because alternate-screen programs
  // manage their own transcript. Skip entirely and wait for a layout that
  // provides a real answer.
  if (container) {
    const rect = container.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return;
  }

  if (!session.shouldFit()) return;
  const buffer = term.buffer.active;
  const wasFollowing = followingBottom(buffer.viewportY, buffer.baseY);
  const savedViewportY = buffer.viewportY;
  const prevCols = term.cols;
  const prevRows = term.rows;

  fitTerminal();

  // Only send a resize if the dimensions actually changed, to avoid
  // unnecessary SIGWINCHes that can disturb TUI layout mid-render.
  if (term.cols !== prevCols || term.rows !== prevRows) {
    if ((globalThis as Record<string, unknown>).__DEV__ === true) {
      const rect = container?.getBoundingClientRect();
      console.log(`[refit] cols ${prevCols0}→${term.cols} rows ${prevRows0}→${term.rows} container=${rect ? `${rect.width.toFixed(0)}x${rect.height.toFixed(0)}` : "none"} scrollActive=${input.scrollActive()}`);
    }
    lastSentCols = term.cols;
    lastSentRows = term.rows;
    session.sendResize(term.cols, term.rows);
  }

  if (wasFollowing) {
    term.scrollToBottom();
  } else {
    // Restore the scroll position when the user is scrolled up into
    // scrollback. Without this, fitting can clamp or reset the viewport,
    // making the view jump to the top on new output or rotation.
    const delta = viewportRestoreDelta(
      savedViewportY,
      term.buffer.active.viewportY,
      term.buffer.active.baseY,
    );
    if (delta !== 0) term.scrollLines(delta);
  }

  updateFollowingState();
}

function scheduleRefit(): void {
  clearTimeout(refitTimer);
  refitTimer = setTimeout(fitAndResize, REFIT_DEBOUNCE_MS);
}

window.addEventListener("resize", scheduleRefit);
window.visualViewport?.addEventListener("resize", scheduleRefit);

// The ResizeObserver fires after the container has actually laid out at its
// new size, which is the authoritative moment to refit. In the RN host the
// container's fixed positioning tracks the WebView frame automatically, so
// this fires when the native layout (keyboard inset, rotation) settles. The
// host "refit" message is a nudge for non-RN or for WKWebView race cases;
// the observer is the ground truth and always runs, held-or-debounced.
if (container) {
  new ResizeObserver(() => scheduleRefit()).observe(container);
}

// Pan moves are forwarded to the gesture engine without coalescing so the
// velocity estimate sees every sample. The engine's line-quantization
// already bounds viewport updates — sub-line moves produce no scroll.

function onHostMessage(event: Event): void {
  const raw = (event as MessageEvent).data;
  const msg = parseHostMessage(raw);
  if (!msg) return;
  if (msg.type === "pan") {
    input.pan(msg.phase, { x: msg.x, y: msg.y, timeMs: msg.timeMs });
    return;
  }
  if (msg.type === "touch") {
    claimMouseOwnership();
    input.touch(msg.phase, { x: msg.x, y: msg.y, timeMs: msg.timeMs }, msg.touchCount);
    return;
  }
  if (msg.type === "refit") {
    scheduleRefit();
    return;
  }
  if (msg.type === "requestDiag") {
    const screen = document.querySelector<HTMLElement>(".xterm-screen");
    post({
      type: "keyboardDiag",
      layoutHeightPx: window.innerHeight,
      visualHeightPx: window.visualViewport?.height ?? -1,
      containerHeightPx: container?.clientHeight ?? -1,
      cols: term.cols,
      rows: term.rows,
      terminalPixelHeight: screen?.clientHeight ?? -1,
    });
    // Also log to console for Safari Web Inspector
    console.log("[KB DIAG]", JSON.stringify({
      innerHeight: window.innerHeight,
      visualViewport: {
        height: window.visualViewport?.height,
        offsetTop: window.visualViewport?.offsetTop,
        pageTop: window.visualViewport?.pageTop,
      },
      containerHeight: container?.clientHeight,
      scrollY: window.scrollY,
      termCols: term.cols,
      termRows: term.rows,
      screenHeight: screen?.clientHeight,
    }));
    return;
  }
  if (msg.type === "scrollToBottom") {
    term.scrollToBottom();
    updateFollowingState();
    return;
  }
  if (msg.type === "getSelection") {
    post({ type: "selectionText", text: term.getSelection() });
    return;
  }
  if (msg.type === "cancelTouch") {
    input.cancelTouch();
    return;
  }
  if (msg.type === "finishSelectionTouch") {
    const selection = document.getSelection();
    if (!selection || selection.isCollapsed) {
      nativeSelectionTree?.blur();
      term.blur();
    }
    return;
  }
  if (msg.type === "init") {
    term.options.fontSize = msg.fontSize;
    if (msg.fontFamily) term.options.fontFamily = msg.fontFamily;
    if (msg.theme) term.options.theme = msg.theme;
  }
  session.handle(msg);
  if (msg.type === "init") fitAndResize();
}

// iOS delivers host messages on window, Android on document.
window.addEventListener("message", onHostMessage);
document.addEventListener("message" as keyof DocumentEventMap, onHostMessage);

// Track the cols/rows values that were last passed to session.sendResize,
// independently from what the PTY actually applied. Comparing these three
// values — term.cols, lastSentCols, ptySize.cols — names the culprit when
// the PTY and xterm disagree.
let lastSentCols: number | null = null;
let lastSentRows: number | null = null;

const syncPageVisibility = () => session.setPageVisible(document.visibilityState !== "hidden");
document.addEventListener("visibilitychange", syncPageVisibility);
syncPageVisibility();

setInterval(() => {
  const modes = modeSnapshot();
  const socketStats = session.socketStats();
  post(
    session.metricsMessage(renderer, {
      bufferType: modes.bufferType,
      mouseTracking: modes.mouseTracking,
      termCols: term.cols,
      termRows: term.rows,
      lastSentCols,
      lastSentRows,
      ptyCols: socketStats?.ptySize?.cols ?? null,
      ptyRows: socketStats?.ptySize?.rows ?? null,
      resizesSent: socketStats?.resizesSent ?? [],
      lastReplaySnapshot: socketStats?.lastReplaySnapshot ?? false,
    }),
  );
}, METRICS_INTERVAL_MS);

fitAndResize();
post({ type: "ready" });
