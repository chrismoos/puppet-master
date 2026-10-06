import type { SocketConnector, SocketLike } from "@puppet-master/client-core/platform";
import type { PtyFrame } from "@puppet-master/client-core/ws/pty";
import {
  wireViewerSize,
  type RepaintSizeSource,
  type ViewerSizeController,
} from "@puppet-master/client-core/ws/terminalRepaint";
import { applyEchoedPtySize, applyEchoedPtySizeThenWrite } from "@puppet-master/client-core/ws/terminalResize";
import { createTerminalViewerIdentity, TerminalSocket } from "@puppet-master/client-core/ws/terminalSocket";
import { TerminalSizePrompt } from "@puppet-master/client-core/ws/terminalSizePrompt";
import type { TerminalSize } from "@puppet-master/client-core/ws/pty";
import { RESET_SEQUENCE, writeTerminalFrame, type TerminalWriteTarget } from "@puppet-master/client-core/ws/terminalWriter";

import { base64ToBytes } from "../../src/terminal/base64";
import {
  TERMINAL_REPLAY_CAP_BYTES,
  parseHostMessage,
  type HostMessage,
  type TerminalInitMessage,
  type ViewMessage,
} from "../../src/terminal/protocol";
import type { MouseTrackingMode } from "../../src/terminal/scrollRouting";

export interface SessionHooks {
  post(msg: ViewMessage): void;
  openSocket(url: string, subprotocol?: string): SocketLike;
  term: TerminalWriteTarget & RepaintSizeSource & { resize(cols: number, rows: number): void };
  now(): number;
  measureSize?(): TerminalSize | null;
  /** Capture and restore a bottom-relative viewport bookmark around replay's
   * reset-and-rebuild write. The revision prevents a late parse callback
   * from undoing a gesture the reader made while replay was parsing. */
  captureViewport?(): number;
  restoreViewport?(bookmark: number): void;
  viewportRevision?(): number;
}

/**
 * Drives the xterm.js WebView side of the RN/WebView contract: owns the
 * direct terminal WebSocket (PTY bytes never cross the RN bridge), applies
 * replay and live frames through the shared client-core pipeline, and
 * reports status and spike measurements back to the native shell.
 */
export class TerminalViewSession {
  private viewerIdentity = createTerminalViewerIdentity();
  private sizePrompt: TerminalSizePrompt;
  private socket: TerminalSocket | null = null;
  private statusDispose: (() => void) | null = null;
  private viewer: ViewerSizeController | null = null;
  /** The terminal whose lines the surface holds, null before the first init. */
  private surfaceTerminalId: string | null = null;
  private surfaceGeneration: string | null = null;
  /** Frames that arrived while the emulator and the PTY disagreed on size,
   * in arrival order, waiting for the echo that brings them back together. */
  private heldFrames: PtyFrame[] = [];
  /** Whether a frame has been written, after which a size change follows by
   * snapshot instead of reflowing what is on screen. */
  private painted = false;
  /** A size another viewer set, taken when its snapshot arrives. */
  private followSize: { cols: number; rows: number } | null = null;
  private hostVisible = true;
  private pageVisible = true;
  private inputEnabled = true;
  private replayCap = TERMINAL_REPLAY_CAP_BYTES;
  private outputBytes = 0;
  private initAt: number | null = null;
  private connectStartedAt: number | null = null;
  private firstFrameMs: number | null = null;
  private lastInputAt: number | null = null;
  private lastEchoMs: number | null = null;

  constructor(private hooks: SessionHooks) {
    this.sizePrompt = new TerminalSizePrompt(
      () => hooks.measureSize ? hooks.measureSize() : { cols: hooks.term.cols, rows: hooks.term.rows },
      (show) => hooks.post({ type: "sizeMismatch", show }),
      (size) => {
        if (!this.socket || !this.hostVisible || !this.pageVisible) return;
        this.followSize = size;
        this.socket.resize(size.cols, size.rows);
        this.socket.refreshSnapshot();
      },
    );
  }

  handleRaw(raw: unknown): void {
    const msg = parseHostMessage(raw);
    if (msg) this.handle(msg);
  }

  handle(msg: HostMessage): void {
    switch (msg.type) {
      case "init":
        this.init(msg);
        break;
      case "write": {
        const bytes = base64ToBytes(msg.dataBase64);
        if (bytes && bytes.byteLength > 0) this.sendInput(bytes);
        break;
      }
      case "setVisible":
        this.inputEnabled = msg.visible;
        this.hostVisible = msg.visible;
        this.applyVisibility();
        break;
      case "shutdown":
        this.shutdown();
        break;
      case "updateSize":
        if (this.hostVisible && this.pageVisible) this.sizePrompt.update();
        break;
      case "dismissSize":
        this.sizePrompt.dismiss();
        break;
    }
  }

  sendInput(bytes: Uint8Array, submitted = false): void {
    if (!this.inputEnabled || !this.socket) return;
    this.lastInputAt = this.hooks.now();
    this.socket.input(bytes, submitted);
  }

  /** Page visibility as the WebView itself reports it, combined with the
   * native host's setVisible so either side hiding the terminal silences it. */
  setPageVisible(visible: boolean): void {
    this.pageVisible = visible;
    this.applyVisibility();
  }

  private applyVisibility(): void {
    this.viewer?.setVisible(this.hostVisible && this.pageVisible);
  }

  sendResize(cols: number, rows: number): void {
    if (!this.shouldFit()) return;
    this.sizePrompt.requested({ cols, rows });
    this.socket?.resize(cols, rows);
  }

  shouldFit(): boolean {
    this.sizePrompt.localChanged();
    return !this.sizePrompt.blocked();
  }

  retry(): void {
    this.socket?.retry();
  }

  socketStats() {
    return this.socket?.stats() ?? null;
  }

  metricsMessage(
    renderer: "webgl" | "dom",
    screen?: {
      bufferType: "normal" | "alternate";
      mouseTracking: MouseTrackingMode;
      termCols: number;
      termRows: number;
      lastSentCols: number | null;
      lastSentRows: number | null;
      ptyCols: number | null;
      ptyRows: number | null;
      resizesSent: Array<{ cols: number; rows: number; at: number }>;
      lastReplaySnapshot: boolean;
    },
  ): ViewMessage {
    const stats = this.socket?.stats();
    return {
      type: "metrics",
      outputBytes: this.outputBytes,
      firstFrameMs: this.firstFrameMs,
      lastEchoMs: this.lastEchoMs,
      renderer,
      inputBytesSent: stats?.inputBytesSent ?? 0,
      inputBytesDropped: stats?.inputBytesDropped ?? 0,
      bufferType: screen?.bufferType,
      mouseTracking: screen?.mouseTracking,
      termCols: screen?.termCols,
      termRows: screen?.termRows,
      lastSentCols: screen?.lastSentCols,
      lastSentRows: screen?.lastSentRows,
      ptyCols: screen?.ptyCols,
      ptyRows: screen?.ptyRows,
      resizesSent: screen?.resizesSent,
      lastReplaySnapshot: screen?.lastReplaySnapshot,
    };
  }

  shutdown(resetPrompt = true): void {
    this.statusDispose?.();
    this.statusDispose = null;
    this.viewer?.dispose();
    this.viewer = null;
    if (resetPrompt) this.sizePrompt.reset();
    this.heldFrames = [];
    if (this.socket) {
      this.socket.close();
      this.socket = null;
      this.hooks.post({ type: "status", phase: "closed" });
    }
  }

  private init(msg: TerminalInitMessage): void {
    const sameTerminal = this.surfaceTerminalId === msg.terminalId && this.surfaceGeneration === msg.generation;
    this.shutdown(false);
    if (!sameTerminal) this.sizePrompt.reset();
    // One surface shows every terminal of a session. The replay's reset is
    // what normally removes the previous terminal, but a replay can be late
    // or never arrive, and the reader must not see the new terminal's output
    // under the old one's lines in the meantime.
    if (this.surfaceTerminalId !== null && this.surfaceTerminalId !== msg.terminalId) {
      this.hooks.term.write(RESET_SEQUENCE);
    }
    this.surfaceTerminalId = msg.terminalId;
    this.surfaceGeneration = msg.generation;
    this.replayCap = msg.replayCapBytes;
    this.outputBytes = 0;
    this.firstFrameMs = null;
    this.lastInputAt = null;
    this.lastEchoMs = null;
    this.inputEnabled = true;
    this.hostVisible = true;
    this.initAt = this.hooks.now();
    this.connectStartedAt = this.initAt;
    // Bearer path (iOS): the openSocket hook reads the current access
    // token on every call (including reconnects) through a header provider,
    // so the bearer header is always fresh. No ticket is needed.
    // Ticket path (Android WebView): the one-use ticket is spent on the
    // first connection; reconnects carry no ticket and are refused as
    // unauthenticated, which makes the host mint a fresh one.
    let ticketSpent = false;
    const usesBearer = !!msg.accessToken;
    const connector: SocketConnector = (path, subprotocol) => {
      const suffix =
        !usesBearer && !ticketSpent && msg.ticket
          ? `&ticket=${encodeURIComponent(msg.ticket)}`
          : "";
      ticketSpent = true;
      return this.hooks.openSocket(`${msg.socketBaseUrl}${path}${suffix}`, subprotocol);
    };
    this.hooks.post({ type: "status", phase: "connecting" });
    const initialSize =
      !this.sizePrompt.blocked() && msg.initialCols && msg.initialRows
        ? { cols: msg.initialCols, rows: msg.initialRows }
        : null;
    if (initialSize) this.sizePrompt.requested(initialSize);
    const socket = new TerminalSocket(
      connector,
      BigInt(msg.terminalId),
      BigInt(msg.generation),
      (frame) => this.receive(frame),
      () => this.hooks.post({ type: "error", message: "terminal socket rejected authentication" }),
      null,
      initialSize,
      this.viewerIdentity,
    );
    this.socket = socket;
    socket.onViewerOwnership((ownership) => this.sizePrompt.observe(ownership));
    socket.onPtySize((size) => {
      // Once the screen has content, a size change is taken as a snapshot at
      // the new size: reflowing the phone's copy pushes rows into scrollback
      // that a program repainting on resize then prints again.
      if (this.painted && (this.hooks.term.cols !== size.cols || this.hooks.term.rows !== size.rows)) {
        this.followSize = { cols: size.cols, rows: size.rows };
        socket.refreshSnapshot();
        return;
      }
      applyEchoedPtySize(this.hooks.term, size);
      this.flushHeldFrames();
    });
    this.statusDispose = socket.subscribe((status) => {
      if (status.phase === "online") {
        this.hooks.post({ type: "status", phase: "online" });
        return;
      }
      this.connectStartedAt = this.hooks.now();
      this.hooks.post({
        type: "status",
        phase: "reconnecting",
        detail: status.lastError,
        canRetry: status.canRetry,
      });
    });
    // Init is the phone's open edge for the viewer size policy: the page
    // fitted before the socket existed, so without this assert the PTY would
    // keep whatever width another viewer left it.
    const hooks = this.hooks;
    this.viewer = wireViewerSize(
      {
        resize: (cols, rows) => {
          this.sizePrompt.requested({ cols, rows });
          socket.resize(cols, rows);
        },
        onStatus: (listener) => socket.subscribe(listener),
        ptySize: () => socket.ptySize(),
      },
      {
        get cols() { return hooks.measureSize ? hooks.measureSize()?.cols ?? 0 : hooks.term.cols; },
        get rows() { return hooks.measureSize ? hooks.measureSize()?.rows ?? 0 : hooks.term.rows; },
      },
      { canAssert: () => this.sizePrompt.needsClaim() },
    );
    if (this.pageVisible) this.viewer.opened();
  }

  private receive(frame: PtyFrame): void {
    const now = this.hooks.now();
    if (this.firstFrameMs === null && this.initAt !== null) {
      this.firstFrameMs = now - this.initAt;
    }
    if (frame.replay) {
      const durationMs = this.connectStartedAt === null ? 0 : now - this.connectStartedAt;
      this.hooks.post({
        type: "replay",
        bytes: frame.data.byteLength,
        durationMs,
        snapshot: frame.snapshot ?? false,
        capped: frame.data.byteLength > this.replayCap,
      });
    } else if (this.lastInputAt !== null) {
      this.lastEchoMs = now - this.lastInputAt;
      this.lastInputAt = null;
    }
    this.outputBytes += frame.data.byteLength;
    // A replay rebuilds the screen from nothing, so it supersedes whatever
    // was waiting; a live frame must not overtake the frames ahead of it.
    if (frame.replay) this.heldFrames = [];
    if (this.heldFrames.length > 0 || !this.paint(frame)) this.heldFrames.push(frame);
  }

  /** Writes only at the PTY's echoed size. A frame refused here is held, not
   * dropped: a refused replay would take its reset with it and leave the
   * previous terminal's lines on the shared surface. */
  private paint(frame: PtyFrame): boolean {
    const bookmark = frame.replay ? this.hooks.captureViewport?.() ?? 0 : 0;
    const revision = this.hooks.viewportRevision?.() ?? 0;
    // The snapshot requested to follow a size change resets the screen, so
    // it takes that size first: nothing of the old screen is left to reflow.
    const follow = this.followSize;
    const echoed = this.socket?.ptySize() ?? null;
    if (frame.replay && follow && echoed && follow.cols === echoed.cols && follow.rows === echoed.rows) {
      this.followSize = null;
      applyEchoedPtySize(this.hooks.term, follow);
    }
    return applyEchoedPtySizeThenWrite(this.hooks.term, this.socket?.ptySize() ?? null, () => {
      this.painted = true;
      writeTerminalFrame(
        this.hooks.term,
        frame,
        bookmark,
        (saved) => {
          if (frame.replay) this.hooks.restoreViewport?.(saved);
        },
        frame.replay ? () => { this.hooks.post({ type: "replayPainted" }); } : undefined,
        () => frame.replay && (this.hooks.viewportRevision?.() ?? 0) === revision,
      );
    });
  }

  private flushHeldFrames(): void {
    while (this.heldFrames.length > 0 && this.paint(this.heldFrames[0])) {
      this.heldFrames.shift();
    }
  }
}
