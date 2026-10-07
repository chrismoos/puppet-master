import { Terminal, type ITheme } from "@xterm/xterm";
import { freshXtermTheme, type TerminalThemeController } from "../theme/controller";
import type { PmClient, PtyHandle } from "@puppet-master/client-core/ws/client";
import type { PtyFrame, TerminalSize } from "@puppet-master/client-core/ws/pty";
import { attachWebglRenderer, type WebglHandle } from "./webglBudget";
import { terminalEvictionPriority } from "@puppet-master/client-core/ws/terminalCache";
import { writeTerminalFrame } from "./terminalWriter";
import { SYNC_OUTPUT_START } from "@puppet-master/client-core/ws/terminalWriter";
import { registerPmTerminalLinkProvider } from "./terminalLinks";
import { isRealPaneFit, measureFit, spawnGeometry, type TerminalGeometry } from "./terminalFit";
import { collectXtermDebug, type TerminalLayerDebug } from "./terminalDiagnostics";
import { wireViewerSize, type ViewerSizeController } from "@puppet-master/client-core/ws/terminalRepaint";
import { applyEchoedPtySize } from "@puppet-master/client-core/ws/terminalResize";
import { applyMeasuredFit, deliverLayerFrame, isInitialSnapshotFrame, showLayer, waitForTerminalRender } from "./terminalLayer";
import { wireTransientTerminalScrollbar } from "./terminalScrollbar";
import { TerminalClipboard } from "./terminalClipboard";
import { registerForegroundCommandHandler, wireTerminalClipboard } from "./terminalHostWiring";
import { documentVisible, watchDocumentVisibility } from "./documentVisibility";
import { createTerminalSizePrompt } from "./terminalSizePrompt";
import type { TerminalSizePrompt } from "@puppet-master/client-core/ws/terminalSizePrompt";
import {
  captureTerminalViewport,
  loadTerminalViewport,
  restoreTerminalViewport,
  saveTerminalViewport,
  type TerminalViewportBookmark,
} from "./terminalViewport";

const SCROLLBACK_LINES = 5_000;
const FONT_SIZE_PX = 13;
const FAST_SCROLL_LINES = 8;

/** Keeps recently viewed terminals attached so switching back is instant. */
const MAX_WARM_TERMINALS = 8;

const MONO_FONT_STACK =
  '"JetBrains Mono Variable", ui-monospace, "SF Mono", Menlo, Consolas, "DejaVu Sans Mono", monospace';

interface Layer {
  key: string;
  sessionId: bigint;
  addressedTerminal: boolean;
  el: HTMLDivElement;
  statusEl: HTMLDivElement;
  term: Terminal;
  webgl: WebglHandle | null;
  handle: PtyHandle | null;
  viewer: ViewerSizeController | null;
  sizePrompt: TerminalSizePrompt;
  disposers: Array<() => void>;
  lastResize: TerminalSize | null;
  lastShownAt: number;
  viewport: TerminalViewportBookmark;
  viewportRevision: number;
  pendingWrites: number;
  /** Parsed live output not yet acknowledged to the controller. */
  unackedBytes: number;
  ackTimer: ReturnType<typeof setTimeout> | null;
  initialReplayPending: boolean;
  /** Shown, but kept behind the previous screen until a snapshot at the
   * pane's size has painted, so a switch is one swap. */
  swapPending: boolean;
  /** What had focus when this layer was shown and held back. */
  focusAtShow: Element | null;
  replayPainted: boolean;
  heldSnapshot: PtyFrame | null;
  /** A width the PTY was asked for whose snapshot has not arrived yet. The
   * emulator keeps its old size and screen until then, so a width change
   * is one swap instead of a blank pane. */
  pendingFit: TerminalSize | null;
  /** The pending size came from another viewer, which this one follows. */
  pendingFollow: boolean;
  /** The latest size measured while a width change was in flight, sent once
   * the size stops changing so a dragged window is not a stream of resizes. */
  settlingFit: TerminalSize | null;
  settleTimer: ReturnType<typeof setTimeout> | null;
  revealTimeout: ReturnType<typeof setTimeout> | null;
  revealRender: { dispose(): void } | null;
}

type TerminalCommandListener = (terminalId: bigint, command: string) => void;

/**
 * Owns the live xterm instances outside React's render path. PTY bytes
 * are written straight to the terminal; only session metadata drives
 * React. Layers stay warm and attached until evicted so returning to a
 * session repaints instantly.
 */
let activeStage: TerminalStage | null = null;

const SWAP_RECHECK_MS = 2_000;
const INITIAL_RECHECK_MS = 250;
/** How long a dragged window's size must hold before it is sent. */
const WIDTH_SETTLE_MS = 100;
const ACK_BATCH_BYTES = 4 * 1024;
const ACK_BATCH_MS = 8;

export class TerminalStage {
  private root: HTMLDivElement;
  private layers = new Map<string, Layer>();
  private observer: ResizeObserver;
  private resizeFrame = 0;
  private visibleId: string | null = null;
  private visibilityDispose: (() => void) | null = null;
  private focusDispose: (() => void) | null = null;
  private terminalCommands = new Map<string, string>();
  private terminalCommandListeners = new Set<TerminalCommandListener>();
  private theme: ITheme;
  private clipboard: TerminalClipboard;

  constructor(private client: PmClient, themeController: TerminalThemeController) {
    this.root = document.createElement("div");
    this.root.className = "term-stage";
    this.clipboard = new TerminalClipboard(document);
    this.root.appendChild(this.clipboard.el as HTMLElement);
    // Browser tests and live debugging read real xterm buffers through this.
    (window as unknown as { __pmStage?: TerminalStage }).__pmStage = this;
    activeStage = this;
    this.observer = new ResizeObserver(() => this.scheduleRefitVisible());
    this.theme = themeController.currentXtermTheme();
    // The controller and stage have the same ConnectedApp lifetime. Keep the
    // registration through React StrictMode's setup/cleanup rehearsal, which
    // calls disposeAll() without replacing this ref-backed stage instance.
    themeController.register(this);
  }

  /** Changes xterm color options in place. It deliberately performs no fit,
   * renderer reload, write, reset, focus, or PTY operation. */
  setTheme(theme: ITheme): void {
    this.theme = freshXtermTheme(theme);
    for (const layer of this.layers.values()) {
      layer.term.options.theme = freshXtermTheme(theme);
    }
  }

  mount(host: HTMLElement): void {
    if (this.root.parentElement !== host) host.appendChild(this.root);
    this.observer.observe(this.root);
    this.visibilityDispose ??= watchDocumentVisibility(document, (visible) => this.pageVisibilityChanged(visible));
    // Page visibility does not cover a window that was never hidden: on a
    // second monitor, or beside another app, focus leaves and returns with
    // visibilityState stuck at "visible", so no reveal happens and the PTY
    // keeps whatever size another viewer last set.
    if (!this.focusDispose) {
      const onFocus = () => this.windowFocused();
      window.addEventListener("focus", onFocus);
      this.focusDispose = () => window.removeEventListener("focus", onFocus);
    }
    this.clipboard.attach();
    this.scheduleRefitVisible();
  }

  unmount(host?: HTMLElement): void {
    if (host && this.root.parentElement !== host) return;
    this.observer.unobserve(this.root);
    this.visibilityDispose?.();
    this.visibilityDispose = null;
    this.focusDispose?.();
    this.focusDispose = null;
    this.clipboard.detach();
    for (const layer of this.layers.values()) {
      layer.viewer?.setVisible(false);
      layer.webgl?.setVisible(false);
    }
    if (this.visibleId) {
      const layer = this.layers.get(this.visibleId);
      if (layer) {
        layer.el.style.visibility = "hidden";
        layer.el.style.pointerEvents = "none";
      }
    }
    this.visibleId = null;
    this.root.remove();
  }

  show(sessionId: bigint): void {
    this.showAddress(sessionId, false);
  }

  showTerminal(terminalId: bigint): void { this.showAddress(terminalId, true); }

  disposeTerminal(terminalId: bigint): void {
    this.disposeLayer(`t:${terminalId}`);
  }

  disposeSession(sessionId: bigint): void {
    this.disposeLayer(`s:${sessionId}`);
  }

  terminalCommand(terminalId: bigint): string | undefined {
    return this.terminalCommands.get(terminalId.toString());
  }

  onTerminalCommand(listener: TerminalCommandListener): () => void {
    this.terminalCommandListeners.add(listener);
    return () => this.terminalCommandListeners.delete(listener);
  }

  /** Live state of every warm layer for the terminal debug bar. */
  debugSnapshot(): TerminalLayerDebug[] {
    const snapshots: TerminalLayerDebug[] = [];
    for (const [key, layer] of this.layers) {
      const buffer = layer.term.buffer.active;
      let stats = null;
      try {
        stats = layer.handle?.stats() ?? null;
      } catch {
        stats = null;
      }
      snapshots.push({
        key,
        visible: key === this.visibleId,
        cols: layer.term.cols,
        rows: layer.term.rows,
        bufferType: buffer.type,
        bufferLines: buffer.length,
        baseY: buffer.baseY,
        viewportY: buffer.viewportY,
         backgroundBytes: 0,
        lastPtyResize: layer.lastResize,
        socket: stats,
        ...collectXtermDebug(layer.term),
      });
    }
    snapshots.sort((a, b) => Number(b.visible) - Number(a.visible) || a.key.localeCompare(b.key));
    return snapshots;
  }

  private showAddress(id: bigint, terminal: boolean): void {
    const key = `${terminal ? "t" : "s"}:${id}`;
    const existing = this.layers.get(key);
    const layer = existing ?? this.createLayer(id, terminal);
    const wasVisibleId = this.visibleId;
    this.visibleId = key;
    // Release the session eviction victim before the GPU budget chooses a different renderer.
    this.evict();
    layer.webgl?.setVisible(true);
    this.tryBindLayerSocket(layer);
    const previous = wasVisibleId !== null && wasVisibleId !== key ? this.layers.get(wasVisibleId) : undefined;
    if (previous && wasVisibleId) this.rememberViewport(wasVisibleId, previous);
    for (const [otherKey, other] of this.layers) {
      if (otherKey !== key) other.viewer?.setVisible(false);
    }
    if (wasVisibleId !== key) {
      layer.sizePrompt.reset();
      if (layer.handle && !layer.initialReplayPending && documentVisible(document)) layer.sizePrompt.update();
    }
    // Showing a layer is the open/switch edge of the viewer size policy in
    // wireViewerSize: this viewer now sets the PTY size, and the warm layers
    // it hid stay silent until they are shown again.
    if (documentVisible(document)) layer.viewer?.opened();
    else layer.viewer?.setVisible(false);
    layer.lastShownAt = Date.now();
    this.layoutVisibleLayer(layer);

    if (layer.initialReplayPending || layer.pendingFit) {
      // The new screen is not ready: keep the previous one on screen, inert,
      // until the snapshot has painted, then swap in one step.
      if (!layer.initialReplayPending) layer.swapPending = true;
      layer.focusAtShow = document.activeElement;
      for (const [otherKey, other] of this.layers) {
        const keep = otherKey === wasVisibleId && otherKey !== key && other.el.style.visibility === "visible";
        other.el.style.visibility = keep ? "visible" : "hidden";
        other.el.style.pointerEvents = "none";
        other.webgl?.setVisible(keep || otherKey === key, true);
      }
      this.scheduleRefitVisible();
      if (layer.revealTimeout === null) {
        layer.revealTimeout = setTimeout(() => {
          layer.revealTimeout = null;
          this.revealPendingLayer(layer);
        }, layer.swapPending ? SWAP_RECHECK_MS : INITIAL_RECHECK_MS);
      }
      this.evict();
      return;
    }

    for (const [otherKey, other] of this.layers) {
      const visible = otherKey === key;
      other.el.style.visibility = visible ? "visible" : "hidden";
      other.el.style.pointerEvents = visible ? "auto" : "none";
      other.webgl?.setVisible(visible, true);
      if (!visible) other.viewer?.setVisible(false);
    }
    const viewport = layer.viewport;
    const viewportRevision = layer.viewportRevision;
    layer.term.focus();
    // A show can coincide with React attaching the shared stage into a host
    // whose layout is still settling. Keep the synchronous fit for the first
    // paint, then reconcile once after layout and once after xterm has parsed
    // everything already queued for this layer. ResizeObserver alone is not
    // sufficient when detach/reattach ends at the same observed dimensions.
    this.scheduleRefitVisible();
    layer.term.write("", () => this.reconcileViewportAfterWrites(
      key,
      layer,
      viewport,
      viewportRevision,
    ));
    this.evict();
  }

  disposeAll(): void {
    this.observer.disconnect();
    this.visibilityDispose?.();
    this.visibilityDispose = null;
    this.focusDispose?.();
    this.focusDispose = null;
    this.clipboard.detach();
    cancelAnimationFrame(this.resizeFrame);
    this.resizeFrame = 0;
    for (const key of [...this.layers.keys()]) this.disposeLayer(key);
    if (activeStage === this) activeStage = null;
    this.root.remove();
  }

  private createLayer(sessionId: bigint, addressedTerminal = false): Layer {
    const key = `${addressedTerminal ? "t" : "s"}:${sessionId}`;
    const el = document.createElement("div");
    el.className = "term-layer";
    el.style.visibility = "hidden";
    el.style.pointerEvents = "none";
    this.root.appendChild(el);

    const term = new Terminal({
      cursorBlink: true,
      macOptionClickForcesSelection: true,
      fontSize: FONT_SIZE_PX,
      fontFamily: MONO_FONT_STACK,
      scrollback: SCROLLBACK_LINES,
      fastScrollSensitivity: FAST_SCROLL_LINES,
      theme: freshXtermTheme(this.theme),
    });
    term.open(el);
    const scrollbarDispose = wireTransientTerminalScrollbar(el);
    let prepareNavigation = () => {};
    const pmLinks = registerPmTerminalLinkProvider(term, this.client, {
      kind: addressedTerminal ? "terminal" : "session",
      id: sessionId.toString(),
    }, () => prepareNavigation());

    const webgl = attachWebglRenderer(term, undefined, false);
    const statusEl = document.createElement("div");
    statusEl.className = "terminal-stream-status";
    statusEl.hidden = true;
    el.appendChild(statusEl);

    const encoder = new TextEncoder();
    const dataSub = term.onData((text) => {
      this.inputTarget(key)?.handle?.input(encoder.encode(text), text === "\r");
    });
    const binarySub = term.onBinary((chunk) => {
      const handle = this.inputTarget(key)?.handle;
      if (!handle) return;
      const bytes = new Uint8Array(chunk.length);
      for (let i = 0; i < chunk.length; i++) bytes[i] = chunk.charCodeAt(i);
      handle.input(bytes);
    });
    const titleSub = addressedTerminal
      ? registerForegroundCommandHandler(term, (command) => this.updateTerminalCommand(sessionId, command))
      : null;
    const clipboardDispose = wireTerminalClipboard(term, el, this.clipboard);

    const sizePrompt = createTerminalSizePrompt(el, () => measureFit(term), (size) => {
      const current = this.layers.get(key);
      if (!current?.handle || this.visibleId !== key || !documentVisible(document)) return;
      const ptySize = current.handle.ptySize();
      const unchanged = current.replayPainted && !current.initialReplayPending
        && !current.pendingFit && !current.pendingFollow
        && current.term.cols === size.cols && current.term.rows === size.rows
        && ptySize?.cols === size.cols && ptySize.rows === size.rows;
      current.pendingFollow = false;
      if (!unchanged) current.pendingFit = size;
      current.lastResize = size;
      current.handle.resize(size.cols, size.rows);
      if (!unchanged) current.handle.refreshSnapshot();
    });
    const layer: Layer = {
      key,
      sessionId,
      addressedTerminal,
      el,
      statusEl,
      term,
      webgl,
      handle: null,
      viewer: null,
      sizePrompt,
      lastResize: null,
      viewport: loadTerminalViewport(key) ?? captureTerminalViewport(term),
      viewportRevision: 0,
      pendingWrites: 0,
      unackedBytes: 0,
      ackTimer: null,
      initialReplayPending: true,
      swapPending: false,
      focusAtShow: null,
      replayPainted: false,
      heldSnapshot: null,
      pendingFit: null,
      pendingFollow: false,
      settlingFit: null,
      settleTimer: null,
      revealTimeout: null,
      revealRender: null,
      disposers: [
        () => sizePrompt.dispose(),
        () => dataSub.dispose(),
        () => binarySub.dispose(),
        () => titleSub?.dispose(),
        clipboardDispose,
        () => pmLinks.dispose(),
        scrollbarDispose,
      ],
      lastShownAt: Date.now(),
    };
    prepareNavigation = () => {
      if (this.visibleId !== key) return;
      layer.viewportRevision += 1;
      this.rememberViewport(key, layer);
    };
    const scrollSub = term.onScroll(() => {
      if (this.visibleId !== key || layer.pendingWrites > 0) return;
      this.rememberViewport(key, layer);
    });
    const rememberUserViewport = () => {
      if (this.visibleId !== key) return;
      layer.viewportRevision += 1;
      this.rememberViewport(key, layer);
    };
    const rememberKeyboardViewport = (event: KeyboardEvent) => {
      if (!["PageUp", "PageDown", "Home", "End"].includes(event.key)) return;
      rememberUserViewport();
    };
    el.addEventListener("wheel", rememberUserViewport, { passive: true });
    el.addEventListener("pointerup", rememberUserViewport);
    el.addEventListener("keyup", rememberKeyboardViewport);
    layer.disposers.push(() => scrollSub.dispose());
    layer.disposers.push(() => el.removeEventListener("wheel", rememberUserViewport));
    layer.disposers.push(() => el.removeEventListener("pointerup", rememberUserViewport));
    layer.disposers.push(() => el.removeEventListener("keyup", rememberKeyboardViewport));
    this.layers.set(key, layer);
    return layer;
  }

  private receiveFrame(key: string, frame: PtyFrame): void {
    const layer = this.layers.get(key);
    if (!layer) return;
    this.writeFrame(layer, frame);
  }

  /** Acks are batched: at once past a few KiB, otherwise within one frame. */
  private acknowledgeParsed(layer: Layer, bytes: number): void {
    layer.unackedBytes += bytes;
    const flush = () => {
      layer.ackTimer = null;
      const pending = layer.unackedBytes;
      layer.unackedBytes = 0;
      layer.handle?.ack(pending);
    };
    if (layer.unackedBytes >= ACK_BATCH_BYTES) {
      if (layer.ackTimer !== null) clearTimeout(layer.ackTimer);
      flush();
    } else if (layer.ackTimer === null) {
      layer.ackTimer = setTimeout(flush, ACK_BATCH_MS);
    }
  }

  private writeFrame(layer: Layer, frame: PtyFrame): void {
    const viewport = layer.viewport;
    const revision = layer.viewportRevision;
    const paint = () => {
      layer.pendingWrites += 1;
      writeTerminalFrame(
        layer.term,
        frame,
        viewport,
        () => {
          layer.pendingWrites -= 1;
          layer.replayPainted = true;
          if (layer.initialReplayPending || (layer.swapPending && frame.replay)) {
            this.revealPendingLayer(layer);
          }
          if (!frame.replay) this.acknowledgeParsed(layer, frame.data.byteLength);
        },
        () => layer.viewportRevision === revision,
      );
    };
    if (frame.replay && layer.pendingFit) {
      const fit = layer.pendingFit;
      layer.pendingFit = null;
      layer.pendingFollow = false;
      // Synchronized output holds the renderer across the resize and the
      // snapshot write, so the reflowed old screen is never painted.
      layer.term.write(SYNC_OUTPUT_START, () => {
        layer.term.resize(fit.cols, fit.rows);
        paint();
      });
      return;
    }
    if (!isInitialSnapshotFrame(layer.initialReplayPending, frame)) {
      if (!layer.handle) return;
      paint();
      return;
    }
    const measured = measureFit(layer.term);
    if (!layer.handle || !isRealPaneFit(layer.el.clientWidth, measured?.cols ?? null)) {
      layer.heldSnapshot = frame;
      return;
    }
    if (!deliverLayerFrame(layer.term, layer.handle.ptySize(), paint)) {
      layer.heldSnapshot = frame;
    }
  }

  private layoutVisibleLayer(layer: Layer): void {
    const localChanged = layer.sizePrompt.localChanged();
    if (layer.sizePrompt.blocked() && !localChanged) {
      this.flushHeldSnapshot(layer);
      return;
    }
    if (this.deferResize(layer)) {
      this.flushHeldSnapshot(layer);
      return;
    }
    let didReset = false;
    showLayer(
      layer.term,
      () => {
        const result = applyMeasuredFit(layer.term, measureFit(layer.term), layer.replayPainted);
        didReset = result === "reset";
      },
      layer.lastResize,
      (cols, rows) => {
        if (!layer.handle) return;
        layer.lastResize = { cols, rows };
        layer.sizePrompt.requested({ cols, rows });
        layer.handle.resize(cols, rows);
      },
    );
    this.tryBindLayerSocket(layer);
    if (didReset && layer.handle) {
      layer.initialReplayPending = true;
      layer.replayPainted = false;
      layer.handle.resync();
    }
    this.flushHeldSnapshot(layer);
  }

  /** A painted terminal changing size asks the PTY and the relay for the new
   * size and keeps showing the old screen until the snapshot arrives. Letting
   * xterm reflow instead pushes rows into its scrollback that a program which
   * repaints on resize then prints again. */
  private deferResize(layer: Layer): boolean {
    const handle = layer.handle;
    if (!handle || !layer.replayPainted || layer.initialReplayPending) return false;
    // Following another viewer's size: this viewer asserts nothing until the
    // snapshot at that size has landed.
    if (layer.pendingFollow) return true;
    const measured = measureFit(layer.term);
    if (!measured || !isRealPaneFit(layer.el.clientWidth, measured.cols)) return false;
    const term = layer.term;
    if (measured.cols === term.cols && measured.rows === term.rows) {
      if (!layer.pendingFit) return false;
      // Back to the size already on screen before the snapshot came.
      layer.pendingFit = null;
      layer.lastResize = { cols: measured.cols, rows: measured.rows };
      layer.sizePrompt.requested(measured);
      handle.resize(measured.cols, measured.rows);
      handle.refreshSnapshot();
      return true;
    }
    const pending = layer.pendingFit;
    if (pending && pending.cols === measured.cols && pending.rows === measured.rows) return true;
    if (pending) {
      layer.settlingFit = { cols: measured.cols, rows: measured.rows };
      if (layer.settleTimer !== null) clearTimeout(layer.settleTimer);
      layer.settleTimer = setTimeout(() => {
        layer.settleTimer = null;
        const settled = layer.settlingFit;
        layer.settlingFit = null;
        if (!settled || !layer.handle) return;
        layer.pendingFit = settled;
        layer.lastResize = settled;
        layer.sizePrompt.requested(settled);
        layer.handle.resize(settled.cols, settled.rows);
        layer.handle.refreshSnapshot();
      }, WIDTH_SETTLE_MS);
      return true;
    }
    layer.pendingFit = { cols: measured.cols, rows: measured.rows };
    layer.lastResize = layer.pendingFit;
    layer.sizePrompt.requested(measured);
    handle.resize(measured.cols, measured.rows);
    handle.refreshSnapshot();
    return true;
  }

  private refitVisible(): void {
    if (!this.root.isConnected || !this.visibleId) return;
    const layer = this.layers.get(this.visibleId);
    if (!layer) return;
    this.layoutVisibleLayer(layer);
  }

  private windowFocused(): void {
    if (!this.root.isConnected || !this.visibleId) return;
    if (!documentVisible(document)) return;
    const layer = this.layers.get(this.visibleId);
    if (!layer) return;
    // Measured before claiming: the window may have been resized while it
    // was blurred.
    this.layoutVisibleLayer(layer);
    layer.viewer?.refocused();
  }

  private pageVisibilityChanged(visible: boolean): void {
    if (!visible) {
      for (const layer of this.layers.values()) {
        layer.viewer?.setVisible(false);
      }
      return;
    }
    if (!this.root.isConnected || !this.visibleId) return;
    this.layers.get(this.visibleId)?.viewer?.setVisible(true);
  }

  private scheduleRefitVisible(): void {
    if (this.resizeFrame) return;
    this.resizeFrame = requestAnimationFrame(() => {
      this.resizeFrame = 0;
      this.refitVisible();
    });
  }

  private reconcileViewportAfterWrites(
    key: string,
    layer: Layer,
    snapshot: TerminalViewportBookmark,
    revision: number,
  ): void {
    if (!this.root.isConnected || this.visibleId !== key || this.layers.get(key) !== layer) return;
    this.layoutVisibleLayer(layer);
    if (layer.viewportRevision !== revision) {
      this.rememberViewport(key, layer);
      return;
    }
    restoreTerminalViewport(layer.term, snapshot);
    layer.viewport = snapshot;
    saveTerminalViewport(key, snapshot);
    layer.term.refresh(0, layer.term.rows - 1);
  }

  private rememberViewport(key: string, layer: Layer): void {
    const viewport = captureTerminalViewport(layer.term);
    layer.viewport = viewport;
    saveTerminalViewport(key, viewport);
  }

  private tryBindLayerSocket(layer: Layer): boolean {
    if (layer.handle) return true;
    const measured = measureFit(layer.term);
    if (!measured || !isRealPaneFit(layer.el.clientWidth, measured.cols)) return false;
    applyMeasuredFit(layer.term, measured, false);
    const size = { cols: measured.cols, rows: measured.rows };
    const handle = layer.addressedTerminal
      ? this.client.openTerminal(layer.sessionId, size)
      : this.client.openPty(layer.sessionId, size);
    layer.handle = handle;
    layer.lastResize = size;
    layer.sizePrompt.requested(size);
    // The size this viewer claims is the one it is moving to: while a resize
    // waits for its snapshot, xterm still holds the old size.
    const viewer = wireViewerSize({
      resize: (cols, rows) => {
        layer.sizePrompt.requested({ cols, rows });
        handle.resize(cols, rows);
      },
      onStatus: (listener) => handle.onStatus(listener),
      ptySize: () => handle.ptySize(),
    }, {
      get cols() { return layer.pendingFit?.cols ?? layer.term.cols; },
      get rows() { return layer.pendingFit?.rows ?? layer.term.rows; },
    }, { canAssert: () => layer.sizePrompt.needsClaim() });
    layer.viewer = viewer;
    const ownershipDispose = handle.onViewerOwnership?.((ownership) => {
      layer.sizePrompt.observe(ownership);
      if (!ownership.local && layer.sizePrompt.blocked() && this.visibleId === layer.key) {
        layer.pendingFit = { cols: ownership.cols, rows: ownership.rows };
        layer.pendingFollow = true;
        handle.refreshSnapshot();
      }
    }) ?? (() => {});
    const ptySizeDispose = handle.onPtySize((sizeEcho) => {
      const current = this.layers.get(layer.key);
      if (!current) return;
      const localSize = measureFit(current.term);
      if (!current.sizePrompt.blocked() && localSize
        && (sizeEcho.cols !== localSize.cols || sizeEcho.rows !== localSize.rows)) return;
      if (current.sizePrompt.blocked()) {
        if (current.settleTimer !== null) clearTimeout(current.settleTimer);
        current.settleTimer = null;
        current.settlingFit = null;
      }
      if (current.handle && current.replayPainted && !current.initialReplayPending && this.visibleId === layer.key) {
        // Another viewer changed the size of the terminal on screen. Follow
        // it with a snapshot at the new size rather than reflowing this copy.
        // A hidden layer reflows quietly and is swapped clean when shown.
        const shown = current.pendingFit ?? { cols: current.term.cols, rows: current.term.rows };
        if (shown.cols !== sizeEcho.cols || shown.rows !== sizeEcho.rows) {
          current.pendingFit = { cols: sizeEcho.cols, rows: sizeEcho.rows };
          current.pendingFollow = true;
          current.handle.refreshSnapshot();
        }
      } else {
        const freeze = current.initialReplayPending || this.visibleId === layer.key;
        applyEchoedPtySize(current.term, sizeEcho, { freeze });
      }
      current.lastResize = sizeEcho;
      this.flushHeldSnapshot(current);
    });
    const statusDispose = handle.onStatus((status) => {
      layer.statusEl.replaceChildren();
      layer.statusEl.hidden = status.phase === "online";
      if (status.phase === "online") return;
      // A slow first snapshot reports itself without an error, and the screen
      // being switched away from stays up through it. A real failure, or a
      // pane with nothing else on it, shows the status instead.
      const currentLayer = this.layers.get(layer.key);
      if (currentLayer && (status.lastError || !this.anotherLayerOnScreen(currentLayer.key))) {
        this.revealPendingLayer(currentLayer, true);
      }
      const label = document.createElement("span");
      label.textContent = status.lastError ?? "reconnecting…";
      layer.statusEl.appendChild(label);
      if (status.canRetry) {
        const retry = document.createElement("button");
        retry.type = "button";
        retry.textContent = "retry";
        retry.addEventListener("click", () => handle.retry());
        layer.statusEl.appendChild(retry);
      }
    });
    layer.disposers.push(statusDispose, ownershipDispose, ptySizeDispose, () => viewer.dispose());
    if (documentVisible(document) && this.visibleId === layer.key) viewer.opened();
    else viewer.setVisible(false);
    handle.connect((frame) => this.receiveFrame(layer.key, frame));
    this.flushHeldSnapshot(layer);
    return true;
  }

  private flushHeldSnapshot(layer: Layer): void {
    const frame = layer.heldSnapshot;
    if (!frame) return;
    layer.heldSnapshot = null;
    this.writeFrame(layer, frame);
  }

  private evict(): void {
    if (this.layers.size <= MAX_WARM_TERMINALS) return;
    const candidates = [...this.layers.entries()]
      .filter(([id, layer]) => id !== this.visibleId && layer.el.style.visibility !== "visible")
      .sort((a, b) => {
        const kind = terminalEvictionPriority(a[0]) - terminalEvictionPriority(b[0]);
        return kind || a[1].lastShownAt - b[1].lastShownAt;
      });
    let over = this.layers.size - MAX_WARM_TERMINALS;
    for (const [id] of candidates) {
      if (over <= 0) break;
      this.disposeLayer(id);
      over -= 1;
    }
  }

  /** A screen kept up while the next session loads still holds keyboard
   * focus, so what is typed into it goes to the session switched to. */
  private inputTarget(key: string): Layer | undefined {
    const target = this.visibleId !== null && this.visibleId !== key && this.layers.get(this.visibleId)
      ? this.visibleId
      : key;
    return this.layers.get(target);
  }

  private anotherLayerOnScreen(key: string): boolean {
    for (const [otherKey, other] of this.layers) {
      if (otherKey !== key && other.el.style.visibility === "visible") return true;
    }
    return false;
  }

  private revealPendingLayer(layer: Layer, force = false): void {
    if (!layer.initialReplayPending && !layer.swapPending) return;
    if (force) {
      layer.revealRender?.dispose();
      layer.revealRender = null;
      this.finishLayerReveal(layer);
      return;
    }
    if (!layer.replayPainted || layer.pendingFit || layer.pendingWrites > 0) return;
    if (layer.revealRender) return;
    layer.revealRender = waitForTerminalRender(
      layer.term,
      () => !layer.pendingFit && layer.pendingWrites === 0 && !layer.term.modes.synchronizedOutputMode,
      () => {
        layer.revealRender = null;
        this.finishLayerReveal(layer);
      },
    );
  }

  private finishLayerReveal(layer: Layer): void {
    layer.initialReplayPending = false;
    layer.swapPending = false;
    if (layer.revealTimeout !== null) {
      clearTimeout(layer.revealTimeout);
      layer.revealTimeout = null;
    }
    if (this.visibleId !== layer.key) return;
    for (const [otherKey, other] of this.layers) {
      if (otherKey !== layer.key) {
        other.el.style.visibility = "hidden";
        other.el.style.pointerEvents = "none";
        other.webgl?.setVisible(false, true);
      }
    }
    // Focus moves with the swap unless the reader put it somewhere else while
    // the next session loaded.
    const active = document.activeElement;
    const focusTerminal = active === layer.focusAtShow || !active || active === document.body || this.root.contains(active);
    layer.focusAtShow = null;
    layer.webgl?.setVisible(true);
    layer.el.style.visibility = "visible";
    layer.el.style.pointerEvents = "auto";
    this.layoutVisibleLayer(layer);
    restoreTerminalViewport(layer.term, layer.viewport);
    layer.term.refresh(0, layer.term.rows - 1);
    if (focusTerminal) layer.term.focus();
  }

  private disposeLayer(key: string): void {
    const layer = this.layers.get(key);
    if (!layer) return;
    if (layer.settleTimer !== null) {
      clearTimeout(layer.settleTimer);
      layer.settleTimer = null;
    }
    if (layer.revealTimeout !== null) {
      clearTimeout(layer.revealTimeout);
      layer.revealTimeout = null;
    }
    layer.revealRender?.dispose();
    this.layers.delete(key);
    for (const dispose of layer.disposers) dispose();
    layer.handle?.close();
    layer.webgl?.dispose();
    layer.term.dispose();
    layer.el.remove();
    if (this.visibleId === key) this.visibleId = null;
  }

  private updateTerminalCommand(terminalId: bigint, command: string): void {
    const key = terminalId.toString();
    const normalized = command.trim();
    if (normalized) this.terminalCommands.set(key, normalized);
    else this.terminalCommands.delete(key);
    for (const listener of this.terminalCommandListeners) listener(terminalId, normalized);
  }

  currentSize(): TerminalGeometry | null {
    return spawnGeometry(this.panesBySizeAuthority(), () => this.measureTransientProbe());
  }

  /** The visible pane first: it is the one whose size the user can see. */
  private *panesBySizeAuthority(): Generator<Terminal> {
    const visible = this.visibleId ? this.layers.get(this.visibleId) : undefined;
    if (visible) yield visible.term;
    for (const layer of this.layers.values()) if (layer !== visible) yield layer.term;
  }

  /** Sizes a spawn before any pane is mounted. The probe lives only for the
   * duration of this call: an xterm left attached to the stage would answer
   * to every pane selector as a second terminal. */
  private measureTransientProbe(): TerminalGeometry | null {
    if (!this.root.isConnected) return null;
    const el = document.createElement("div");
    el.className = "term-probe";
    el.style.position = "absolute";
    el.style.inset = "0";
    el.style.overflow = "hidden";
    el.style.visibility = "hidden";
    el.style.pointerEvents = "none";
    this.root.appendChild(el);
    const term = new Terminal({
      fontSize: FONT_SIZE_PX,
      fontFamily: MONO_FONT_STACK,
      scrollback: 1,
    });
    try {
      term.open(el);
      const measured = measureFit(term);
      if (!measured || !isRealPaneFit(this.root.clientWidth, measured.cols)) return null;
      return { cols: measured.cols, rows: measured.rows };
    } finally {
      term.dispose();
      el.remove();
    }
  }
}

export function currentTerminalGeometry(): TerminalGeometry | null {
  return activeStage?.currentSize() ?? null;
}
