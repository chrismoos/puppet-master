import { SOCKET_OPEN, type SocketConnector, type SocketLike } from "../platform";
import type { PtySink, TerminalSize, TerminalOwnership } from "./pty";
import {
  decodeTerminalOutput,
  decodeTerminalResize,
  decodeTerminalOwnership,
  encodeTerminalResizeRequest,
  encodeTerminalAck,
  encodeTerminalResync,
  encodeTerminalInput,
  encodeTerminalResize,
  TERMINAL_FLAG_REPLAY,
  TERMINAL_FLAG_REPLAY_END,
  TERMINAL_FLAG_REPLAY_SNAPSHOT,
  TERMINAL_FLAG_REPLAY_START,
  TERMINAL_SUBPROTOCOL,
} from "./terminalFrame";

const RECONNECT_MIN_DELAY_MS = 250;
export const MAX_PENDING_INPUT_BYTES = 64 * 1024;
const RECONNECT_MAX_DELAY_MS = 4_000;
const RECONNECT_BACKOFF_FACTOR = 2;
const RECONNECT_JITTER = 0.2;
const STATUS_DELAY_MS = 750;
const MANUAL_RETRY_DELAY_MS = 10_000;
const REPLAY_TIMEOUT_MS = 10_000;

export type TerminalStreamStatus =
  | { phase: "online" }
  | { phase: "reconnecting"; canRetry: boolean; lastError?: string };

/** Live counters for the terminal debug bar. */
export interface TerminalSocketStats {
  generation: string;
  phase: "online" | "reconnecting";
  socketsOpened: number;
  socketsClosed: number;
  outputBytes: number;
  lastOutputAt: number | null;
  replayCount: number;
  lastReplayBytes: number;
  lastReplayAt: number | null;
  inputBytesSent: number;
  inputBytesPending: number;
  inputBytesDropped: number;
  resizesSent: Array<{ cols: number; rows: number; at: number }>;
  /** The PTY's actual size as last echoed by the daemon. */
  ptySize: TerminalSize | null;
  lastReplaySnapshot: boolean;
  lastError: string;
}

const STATS_RESIZE_HISTORY = 6;

type StatusListener = (status: TerminalStreamStatus) => void;

type PtySizeListener = (size: TerminalSize) => void;

interface PendingInput {
  data: Uint8Array;
  submitted: boolean;
}

export interface TerminalViewerIdentity { id: bigint; request: bigint }

export function createTerminalViewerIdentity(): TerminalViewerIdentity {
  const random = new Uint32Array(2);
  const { crypto } = globalThis as typeof globalThis & {
    crypto?: { getRandomValues(values: Uint32Array): Uint32Array };
  };
  if (crypto?.getRandomValues) crypto.getRandomValues(random);
  else {
    random[0] = Math.floor(Math.random() * 2 ** 32);
    random[1] = Math.floor(Math.random() * 2 ** 32);
  }
  return { id: (BigInt(random[0]) << 32n) | BigInt(random[1]) | 1n, request: 0n };
}

export class TerminalSocket {
  private socket: SocketLike | null = null;
  private stopped = false;
  private available = true;
  private replayComplete = false;
  private reconnectDelayMs = RECONNECT_MIN_DELAY_MS;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private statusTimer: ReturnType<typeof setTimeout> | null = null;
  private retryStatusTimer: ReturnType<typeof setTimeout> | null = null;
  private replayTimer: ReturnType<typeof setTimeout> | null = null;
  private failureStartedAt: number | null = null;
  private lastError = "";
  private pendingResize: (TerminalSize & { request: bigint }) | null = null;
  private pendingClaim = 0n;
  private ownershipRevision = -1n;
  private ownershipSupported = false;
  private ownershipListeners = new Set<(ownership: TerminalOwnership) => void>();
  private replayChunks: Uint8Array[] = [];
  private replayBytes = 0;
  private replaySnapshot = false;
  private snapshotPending = false;
  private lastReplaySnapshot = false;
  private listeners = new Set<StatusListener>();
  private currentPtySize: TerminalSize | null = null;
  private ptySizeListeners = new Set<PtySizeListener>();
  private socketsOpened = 0;
  private socketsClosed = 0;
  private outputBytes = 0;
  private lastOutputAt: number | null = null;
  private replayCount = 0;
  private lastReplayBytes = 0;
  private lastReplayAt: number | null = null;
  private inputBytesSent = 0;
  private inputBytesDropped = 0;
  private pendingInput: PendingInput[] = [];
  private pendingInputBytes = 0;
  private pendingLive: Uint8Array[] = [];
  private resizesSent: Array<{ cols: number; rows: number; at: number }> = [];

  constructor(
    private connector: SocketConnector,
    private terminalId: bigint,
    private generation: bigint,
    private sink: PtySink,
    private unauthenticated: () => void,
    unavailableReason: string | null = null,
    private initialSize?: TerminalSize | null,
    private viewer = createTerminalViewerIdentity(),
  ) {
    if (unavailableReason) {
      this.available = false;
      this.failureStartedAt = Date.now();
      this.lastError = unavailableReason;
    } else {
      this.connect();
    }
  }

  /** A terminal the user can see and type into must deliver what they typed,
   * so bytes offered before the stream is ready are held rather than lost. */
  input(data: Uint8Array, submitted = false): void {
    const socket = this.socket;
    if (this.stopped) {
      this.inputBytesDropped += data.byteLength;
      return;
    }
    if (!this.replayComplete || socket?.readyState !== SOCKET_OPEN) {
      if (this.pendingInputBytes + data.byteLength > MAX_PENDING_INPUT_BYTES) {
        this.inputBytesDropped += data.byteLength;
        return;
      }
      this.pendingInput.push({ data, submitted });
      this.pendingInputBytes += data.byteLength;
      return;
    }
    this.sendInput(socket, data, submitted);
  }

  /** Tells the controller how many output bytes the terminal has parsed, so a
   * flood is paced to this viewer. Best effort: a closed socket just loses it. */
  ack(bytes: number): void {
    const socket = this.socket;
    if (this.stopped || bytes <= 0 || socket?.readyState !== SOCKET_OPEN) return;
    socket.send(encodeTerminalAck(this.generation, bytes));
  }

  private sendInput(socket: SocketLike, data: Uint8Array, submitted: boolean): void {
    this.inputBytesSent += data.byteLength;
    socket.send(encodeTerminalInput(this.generation, data, submitted));
  }

  private flushPendingInput(socket: SocketLike): void {
    const pending = this.pendingInput;
    this.clearPendingInput();
    for (const entry of pending) this.sendInput(socket, entry.data, entry.submitted);
  }

  private clearPendingInput(): void {
    this.pendingInput = [];
    this.pendingInputBytes = 0;
  }

  /** A size offered while the stream is not online is held until the next
   * replay completes, then delivered once. The socket never re-sends a
   * delivered size on its own: whether a reconnecting viewer re-asserts is
   * the viewer size policy's decision, not the transport's. */
  resize(cols: number, rows: number): void {
    this.initialSize = { cols, rows };
    const request = ++this.viewer.request;
    this.pendingClaim = request;
    this.pendingResize = { cols, rows, request };
    if (!this.replayComplete || this.socket?.readyState !== SOCKET_OPEN) return;
    this.sendResize(this.socket, cols, rows, request);
  }

  private sendResize(socket: SocketLike, cols: number, rows: number, request: bigint): void {
    if (!this.ownershipSupported) {
      this.pendingResize = null;
      this.pendingClaim = 0n;
    }
    if (!this.ownershipSupported && this.currentPtySize && this.currentPtySize.cols === cols && this.currentPtySize.rows === rows) {
      return;
    }
    this.recordResize(cols, rows);
    socket.send(this.ownershipSupported
      ? encodeTerminalResizeRequest(this.generation, request, cols, rows)
      : encodeTerminalResize(this.generation, cols, rows));
  }

  stats(): TerminalSocketStats {
    return {
      generation: this.generation.toString(),
      phase: this.replayComplete ? "online" : "reconnecting",
      socketsOpened: this.socketsOpened,
      socketsClosed: this.socketsClosed,
      outputBytes: this.outputBytes,
      lastOutputAt: this.lastOutputAt,
      replayCount: this.replayCount,
      lastReplayBytes: this.lastReplayBytes,
      lastReplayAt: this.lastReplayAt,
      inputBytesSent: this.inputBytesSent,
      inputBytesPending: this.pendingInputBytes,
      inputBytesDropped: this.inputBytesDropped,
      resizesSent: [...this.resizesSent],
      ptySize: this.currentPtySize,
      lastReplaySnapshot: this.lastReplaySnapshot,
      lastError: this.lastError,
    };
  }

  /** The PTY's actual size as last echoed by the daemon, shared by every
   * viewer of this terminal; null until the first echo arrives. */
  ptySize(): TerminalSize | null {
    return this.currentPtySize;
  }

  onViewerOwnership(listener: (ownership: TerminalOwnership) => void): () => void {
    this.ownershipListeners.add(listener);
    return () => this.ownershipListeners.delete(listener);
  }

  onPtySize(listener: PtySizeListener): () => void {
    this.ptySizeListeners.add(listener);
    return () => this.ptySizeListeners.delete(listener);
  }

  private recordResize(cols: number, rows: number): void {
    this.resizesSent.push({ cols, rows, at: Date.now() });
    if (this.resizesSent.length > STATS_RESIZE_HISTORY) this.resizesSent.shift();
  }

  subscribe(listener: StatusListener): () => void {
    this.listeners.add(listener);
    if (this.replayComplete) listener({ phase: "online" });
    else if (!this.available) listener(this.reconnectingStatus());
    else if (this.failureStartedAt !== null && Date.now() - this.failureStartedAt >= STATUS_DELAY_MS) {
      listener(this.reconnectingStatus());
    }
    return () => this.listeners.delete(listener);
  }

  updateGeneration(generation: bigint): void {
    if (generation <= this.generation) return;
    this.generation = generation;
    // A new generation is a different process, so held bytes aimed at the
    // previous one must not reach it.
    this.inputBytesDropped += this.pendingInputBytes;
    this.clearPendingInput();
    if (this.available) this.reconnectNow();
  }

  setAvailable(available: boolean, reason = "terminal worker unavailable"): void {
    if (this.stopped) return;
    if (available === this.available) {
      if (!available && reason !== this.lastError) {
        this.lastError = reason;
        this.emit(this.reconnectingStatus());
      }
      return;
    }
    this.available = available;
    if (available) {
      this.lastError = "";
      this.failureStartedAt = null;
      this.reconnectNow();
      return;
    }
    this.replayComplete = false;
    this.failureStartedAt = Date.now();
    this.lastError = reason;
    this.clearReconnectTimer();
    this.clearStatusTimers();
    this.clearReplayTimer();
    const socket = this.socket;
    this.socket = null;
    socket?.close();
    this.emit(this.reconnectingStatus());
  }

  retry(): void {
    if (this.stopped || !this.available || this.replayComplete) return;
    this.reconnectNow();
  }

  refreshSnapshot(): void {
    const socket = this.socket;
    if (this.stopped || !this.available) return;
    if (socket && !this.replayComplete) {
      this.snapshotPending = true;
      return;
    }
    if (socket?.readyState !== SOCKET_OPEN) {
      this.resync();
      return;
    }
    socket.send(encodeTerminalResync(this.generation));
  }

  resync(): void {
    if (this.stopped || !this.available) return;
    this.reconnectNow();
  }

  close(): void {
    this.stopped = true;
    this.clearPendingInput();
    this.clearReconnectTimer();
    this.clearStatusTimers();
    this.clearReplayTimer();
    const socket = this.socket;
    this.socket = null;
    socket?.close();
    this.listeners.clear();
    this.ptySizeListeners.clear();
    this.ownershipListeners.clear();
  }

  private connect(): void {
    if (this.stopped || !this.available) return;
    this.socketsOpened += 1;
    this.ownershipRevision = -1n;
    this.ownershipSupported = false;
    let url = `/ws/terminal/${this.terminalId}?generation=${this.generation}&viewer=${this.viewer.id}`;
    const size = this.pendingResize ?? (this.socketsOpened === 1 ? this.initialSize : null);
    if (size && size.cols >= 2 && size.rows >= 1) {
      const request = this.pendingResize?.request ?? ++this.viewer.request;
      this.pendingClaim = request;
      this.pendingResize = { ...size, request };
      url += `&cols=${size.cols}&rows=${size.rows}&claim=${request}`;
    }
    const socket = this.connector(url, TERMINAL_SUBPROTOCOL);
    socket.binaryType = "arraybuffer";
    this.socket = socket;
    this.replayComplete = false;
    this.pendingLive = [];
    this.clearReplay();
    this.beginFailure();
    this.replayTimer = setTimeout(() => {
      if (this.socket !== socket || this.replayComplete) return;
      this.lastError = "terminal replay timed out";
      socket.close();
    }, REPLAY_TIMEOUT_MS);

    socket.onmessage = (event) => {
      if (this.socket !== socket || !(event.data instanceof ArrayBuffer)) return;
      const ownership = decodeTerminalOwnership(event.data);
      if (ownership) {
        if (ownership.generation !== this.generation || ownership.revision < this.ownershipRevision) return;
        this.ownershipSupported = true;
        const wasClaiming = this.pendingClaim !== 0n;
        if (ownership.acknowledgment >= this.pendingClaim) this.pendingClaim = 0n;
        if (this.pendingResize && ownership.acknowledgment >= this.pendingResize.request) this.pendingResize = null;
        const newer = ownership.revision > this.ownershipRevision;
        this.ownershipRevision = ownership.revision;
        if (this.pendingClaim !== 0n || (!newer && !wasClaiming)) return;
        const state = { local: ownership.owner === this.viewer.id, cols: ownership.cols, rows: ownership.rows };
        for (const listener of this.ownershipListeners) listener(state);
        return;
      }
      const resize = decodeTerminalResize(event.data);
      if (resize) {
        if (resize.generation !== this.generation) return;
        const size = { cols: resize.cols, rows: resize.rows };
        this.currentPtySize = size;
        for (const listener of this.ptySizeListeners) listener(size);
        return;
      }
      const frame = decodeTerminalOutput(event.data);
      if (!frame || frame.generation !== this.generation) return;
      if ((frame.flags & TERMINAL_FLAG_REPLAY) !== 0) {
        if ((frame.flags & TERMINAL_FLAG_REPLAY_START) !== 0) {
          this.clearReplay();
        }
        if ((frame.flags & TERMINAL_FLAG_REPLAY_SNAPSHOT) !== 0) {
          this.replaySnapshot = true;
        }
        this.replayChunks.push(frame.data);
        this.replayBytes += frame.data.byteLength;
        if ((frame.flags & TERMINAL_FLAG_REPLAY_END) !== 0) {
          const data = new Uint8Array(this.replayBytes);
          let offset = 0;
          for (const chunk of this.replayChunks) {
            data.set(chunk, offset);
            offset += chunk.byteLength;
          }
          const snapshot = this.replaySnapshot;
          this.clearReplay();
          this.replayCount += 1;
          this.lastReplayBytes = data.byteLength;
          this.lastReplayAt = Date.now();
          this.lastReplaySnapshot = snapshot;
          this.outputBytes += data.byteLength;
          this.lastOutputAt = this.lastReplayAt;
          this.sink({ data, replay: true, snapshot });
          this.flushPendingLive();
          this.markOnline(socket);
        }
        return;
      }
      if (this.replayComplete) {
        this.outputBytes += frame.data.byteLength;
        this.lastOutputAt = Date.now();
        this.sink({ data: frame.data, replay: false });
      } else {
        this.pendingLive.push(frame.data);
      }
    };
    socket.onerror = () => {
      if (this.socket === socket) this.lastError = "terminal connection failed";
    };
    socket.onclose = (event) => {
      if (this.socket !== socket) return;
      this.socketsClosed += 1;
      this.clearReplayTimer();
      this.socket = null;
      this.replayComplete = false;
      this.pendingLive = [];
      this.clearReplay();
      if (event.code === 4401) {
        this.unauthenticated();
        return;
      }
      this.lastError = event.reason || this.lastError || `connection closed (${event.code || 1006})`;
      this.beginFailure();
      if (this.available) this.scheduleReconnect();
    };
  }

  private markOnline(socket: SocketLike): void {
    if (this.socket !== socket) return;
    this.replayComplete = true;
    this.reconnectDelayMs = RECONNECT_MIN_DELAY_MS;
    this.failureStartedAt = null;
    this.lastError = "";
    this.clearStatusTimers();
    this.clearReplayTimer();
    this.flushPendingResizeAfterMatchingPaint(socket);
    this.flushPendingInput(socket);
    if (this.snapshotPending) {
      this.snapshotPending = false;
      socket.send(encodeTerminalResync(this.generation));
    }
    this.emit({ phase: "online" });
  }

  private flushPendingResizeAfterMatchingPaint(socket: SocketLike): void {
    const pendingResize = this.pendingResize;
    if (!pendingResize) return;
    this.sendResize(socket, pendingResize.cols, pendingResize.rows, pendingResize.request);
  }

  private beginFailure(): void {
    if (this.stopped || this.failureStartedAt !== null) return;
    this.failureStartedAt = Date.now();
    this.statusTimer = setTimeout(() => {
      this.statusTimer = null;
      if (!this.replayComplete && !this.stopped) this.emit(this.reconnectingStatus());
    }, STATUS_DELAY_MS);
    this.retryStatusTimer = setTimeout(() => {
      this.retryStatusTimer = null;
      if (!this.replayComplete && !this.stopped) this.emit(this.reconnectingStatus());
    }, MANUAL_RETRY_DELAY_MS);
  }

  private reconnectingStatus(): TerminalStreamStatus {
    if (!this.available) {
      return {
        phase: "reconnecting",
        canRetry: false,
        lastError: this.lastError || "terminal worker unavailable",
      };
    }
    const canRetry = this.failureStartedAt !== null
      && Date.now() - this.failureStartedAt >= MANUAL_RETRY_DELAY_MS;
    return {
      phase: "reconnecting",
      canRetry,
      lastError: canRetry ? this.lastError || "terminal connection unavailable" : undefined,
    };
  }

  private scheduleReconnect(): void {
    if (this.stopped || !this.available || this.reconnectTimer !== null) return;
    const spread = 1 + (Math.random() * 2 - 1) * RECONNECT_JITTER;
    const delay = Math.round(this.reconnectDelayMs * spread);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, delay);
    this.reconnectDelayMs = Math.min(
      this.reconnectDelayMs * RECONNECT_BACKOFF_FACTOR,
      RECONNECT_MAX_DELAY_MS,
    );
  }

  private reconnectNow(): void {
    this.clearReconnectTimer();
    this.clearReplayTimer();
    const socket = this.socket;
    this.socket = null;
    this.replayComplete = false;
    socket?.close();
    this.beginFailure();
    this.connect();
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer === null) return;
    clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
  }

  private clearStatusTimers(): void {
    if (this.statusTimer !== null) clearTimeout(this.statusTimer);
    if (this.retryStatusTimer !== null) clearTimeout(this.retryStatusTimer);
    this.statusTimer = null;
    this.retryStatusTimer = null;
  }

  private clearReplayTimer(): void {
    if (this.replayTimer !== null) clearTimeout(this.replayTimer);
    this.replayTimer = null;
  }

  private flushPendingLive(): void {
    const pending = this.pendingLive;
    this.pendingLive = [];
    for (const data of pending) {
      this.outputBytes += data.byteLength;
      this.lastOutputAt = Date.now();
      this.sink({ data, replay: false });
    }
  }

  private clearReplay(): void {
    this.replayChunks = [];
    this.replayBytes = 0;
    this.replaySnapshot = false;
  }

  private emit(status: TerminalStreamStatus): void {
    for (const listener of this.listeners) listener(status);
  }
}
