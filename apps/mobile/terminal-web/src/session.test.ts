import { encodeTerminalOwnership } from "@puppet-master/client-core/ws/terminalFrame";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SocketCloseEvent, SocketLike, SocketMessageEvent } from "@puppet-master/client-core/platform";
import {
  encodeTerminalResize,
  TERMINAL_FLAG_REPLAY,
  TERMINAL_FLAG_REPLAY_END,
  TERMINAL_FLAG_REPLAY_SNAPSHOT,
  TERMINAL_FLAG_REPLAY_START,
  TERMINAL_TAG_INPUT,
  TERMINAL_TAG_RESIZE,
  TERMINAL_TAG_RESIZE_REQUEST,
  TERMINAL_TAG_RESYNC,
  TERMINAL_SUBPROTOCOL,
} from "@puppet-master/client-core/ws/terminalFrame";
import { VIEWER_SIZE_ASSERT_DEBOUNCE_MS } from "@puppet-master/client-core/ws/terminalRepaint";


import { encodeHostMessage, type ViewMessage } from "../../src/terminal/protocol";
import { TerminalViewSession } from "./session";

class FakeSocket implements SocketLike {
  binaryType = "";
  readyState = 1;
  sent: ArrayBuffer[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: SocketMessageEvent) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: ((event: SocketCloseEvent) => void) | null = null;
  closed = false;

  constructor(
    public url: string,
    public subprotocol: string | undefined,
  ) {}

  send(data: ArrayBufferLike | Uint8Array): void {
    this.sent.push(data instanceof Uint8Array ? data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength) : (data as ArrayBuffer));
  }

  close(): void {
    this.closed = true;
  }

  deliver(frame: ArrayBuffer): void {
    this.onmessage?.({ data: frame });
  }
}

function outputFrame(generation: bigint, flags: number, payload: Uint8Array): ArrayBuffer {
  const frame = new ArrayBuffer(10 + payload.byteLength);
  const view = new DataView(frame);
  view.setUint8(0, 0x01);
  view.setBigUint64(1, generation, true);
  view.setUint8(9, flags);
  new Uint8Array(frame, 10).set(payload);
  return frame;
}

const REPLAY_ALL = TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END;

function harness(localFit?: { cols: number; rows: number }) {
  const sockets: FakeSocket[] = [];
  const posts: ViewMessage[] = [];
  const writes: Uint8Array[] = [];
  let clock = 1_000;
  let viewport = 0;
  let viewportRevision = 0;
  const restoredViewports: number[] = [];
  const term = {
    cols: 80,
    rows: 24,
    resize(cols: number, rows: number) {
      this.cols = cols;
      this.rows = rows;
    },
    write: (data: Uint8Array, callback?: () => void) => {
      writes.push(data);
      callback?.();
    },
  };
  const session = new TerminalViewSession({
    measureSize: localFit ? () => localFit ?? null : undefined,
    post: (msg) => posts.push(msg),
    openSocket: (url, subprotocol) => {
      const socket = new FakeSocket(url, subprotocol);
      sockets.push(socket);
      return socket;
    },
    term,
    now: () => clock,
    captureViewport: () => viewport,
    restoreViewport: (bookmark) => restoredViewports.push(bookmark),
    viewportRevision: () => viewportRevision,
  });
  return {
    session,
    setLocal: (size: { cols: number; rows: number }) => { localFit = size; },
    sockets,
    posts,
    writes,
    term,
    tick: (ms: number) => (clock += ms),
    settle: () => vi.advanceTimersByTime(VIEWER_SIZE_ASSERT_DEBOUNCE_MS),
    setViewport: (value: number) => { viewport = value; },
    moveViewport: () => { viewportRevision += 1; },
    restoredViewports,
  };
}


const LEGACY_COLS_OFFSET = 9;
const REQUEST_COLS_OFFSET = 17;

function resizesSent(socket: FakeSocket): Array<[number, number]> {
  return socket.sent
    .filter((f) => [TERMINAL_TAG_RESIZE, TERMINAL_TAG_RESIZE_REQUEST].includes(new DataView(f).getUint8(0)))
    .map((f) => {
      const view = new DataView(f);
      const colsOffset = view.getUint8(0) === TERMINAL_TAG_RESIZE ? LEGACY_COLS_OFFSET : REQUEST_COLS_OFFSET;
      return [view.getUint16(colsOffset, true), view.getUint16(colsOffset + 2, true)];
    });
}

const INIT = encodeHostMessage({
  type: "init",
  socketBaseUrl: "wss://pm.example",
  terminalId: "7",
  generation: "3",
  ticket: "tick et",
  replayCapBytes: 1024,
  fontSize: 14,
});

const BEARER_INIT = encodeHostMessage({
  type: "init",
  socketBaseUrl: "wss://pm.example",
  terminalId: "7",
  generation: "3",
  accessToken: "bearer-tok-1",
  initialCols: 50,
  initialRows: 20,
  replayCapBytes: 131072,
  fontSize: 14,
});

describe("TerminalViewSession", () => {
  it("offers Update and Dismiss while preserving independently measured mobile dimensions", () => {
    const { session, sockets, posts, term, settle, setLocal } = harness({ cols: 80, rows: 24 });
    session.handleRaw(JSON.stringify({ ...JSON.parse(INIT), initialCols: 80, initialRows: 24 }));
    const socket = sockets[0];
    const query = new URL(socket.url).searchParams;
    const owner = BigInt(query.get("viewer")!);
    socket.deliver(encodeTerminalOwnership({ generation: 3n, revision: 1n, owner, acknowledgment: BigInt(query.get("claim")!), cols: 80, rows: 24 }));
    socket.deliver(encodeTerminalResize(3n, 80, 24));
    socket.deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x68])));
    settle();
    socket.deliver(encodeTerminalOwnership({ generation: 3n, revision: 2n, owner: owner + 1n, acknowledgment: BigInt(query.get("claim")!), cols: 120, rows: 40 }));
    socket.deliver(encodeTerminalResize(3n, 120, 40));
    socket.deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x69])));
    expect([term.cols, term.rows]).toEqual([120, 40]);
    expect(posts).toContainEqual({ type: "sizeMismatch", show: true });
    expect(session.shouldFit()).toBe(false);
    const before = resizesSent(socket).length;
    session.handle({ type: "setVisible", visible: false });
    session.handle({ type: "setVisible", visible: true });
    settle();
    expect(resizesSent(socket)).toHaveLength(before);
    session.handle({ type: "dismissSize" });
    socket.deliver(encodeTerminalResize(3n, 100, 30));
    expect(posts.at(-1)).toEqual({ type: "sizeMismatch", show: false });
    session.handle({ type: "updateSize" });
    expect(resizesSent(socket).at(-1)).toEqual([80, 24]);
    socket.deliver(encodeTerminalResize(3n, 80, 24));
    expect(posts.at(-1)).toEqual({ type: "sizeMismatch", show: false });
    setLocal({ cols: 90, rows: 25 });
    expect(session.shouldFit()).toBe(true);
    session.shutdown();
  });

  beforeEach(() => {
    vi.useFakeTimers();
  });

  it("resizes the emulator to the size echo before writing replay", () => {
    const { session, sockets, term, writes } = harness();
    session.handleRaw(INIT);
    sockets[0].deliver(encodeTerminalResize(3n, 100, 30));
    expect(term.cols).toBe(100);
    expect(term.rows).toBe(30);
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x68])));
    expect(writes.length).toBeGreaterThan(0);
    session.shutdown();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("does not re-present a spent ticket when the socket reconnects", () => {
    // A one-use ticket cannot authenticate a second connect. Re-sending it
    // is refused as already used, and once the daemon prunes the consumed
    // row every retry after that is refused as an unknown ticket — a stream
    // of warnings blaming a cause that is not the real one. Reconnecting
    // without it is refused as unauthenticated, which is the signal that
    // makes the host mint a fresh ticket.
    const { session, sockets } = harness();
    session.handleRaw(INIT);
    expect(sockets).toHaveLength(1);
    expect(sockets[0].url).toContain("ticket=");

    // The socket layer reconnects from the close event, which the fake only
    // flags, so it is delivered explicitly.
    sockets[0].onclose?.({ code: 1006, reason: "" } as never);
    vi.advanceTimersByTime(60_000);

    expect(sockets.length).toBeGreaterThan(1);
    for (const socket of sockets.slice(1)) {
      expect(socket.url).not.toContain("ticket=");
    }
    session.shutdown();
  });

  it("opens the terminal socket directly with generation and ticket in the URL", () => {
    const { session, sockets, posts } = harness();
    session.handleRaw(INIT);
    expect(sockets).toHaveLength(1);
    const url = new URL(sockets[0].url);
    expect(url.origin + url.pathname).toBe("wss://pm.example/ws/terminal/7");
    expect(url.searchParams.get("generation")).toBe("3");
    expect(url.searchParams.get("ticket")).toBe("tick et");
    expect(BigInt(url.searchParams.get("viewer")!)).toBeGreaterThan(0n);
    expect(sockets[0].subprotocol).toBe(TERMINAL_SUBPROTOCOL);
    expect(sockets[0].binaryType).toBe("arraybuffer");
    expect(posts).toContainEqual({ type: "status", phase: "connecting" });
    session.shutdown();
  });

  it("reports replay completion and asserts its size once online", () => {
    const { session, sockets, posts, writes, tick, settle } = harness();
    session.handleRaw(INIT);
    tick(120);
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x68, 0x69])));
    expect(posts).toContainEqual({
      type: "replay",
      bytes: 2,
      durationMs: 120,
      snapshot: false,
      capped: false,
    });
    expect(posts).toContainEqual({ type: "status", phase: "online" });
    expect(writes.length).toBeGreaterThan(0);
    settle();
    expect(resizesSent(sockets[0])).toEqual([[80, 24]]);
    session.shutdown();
  });

  it("restores the captured viewport bookmark after replay parsing", () => {
    const { session, sockets, setViewport, restoredViewports } = harness();
    setViewport(37);
    session.handleRaw(INIT);
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x68])));
    expect(restoredViewports).toEqual([37]);
    session.shutdown();
  });

  it("does not restore over a gesture made while replay is parsing", () => {
    const callbacks: Array<() => void> = [];
    let revision = 0;
    const sockets: FakeSocket[] = [];
    const restored: number[] = [];
    const session = new TerminalViewSession({
      post: () => {},
      openSocket: (url, subprotocol) => {
        const socket = new FakeSocket(url, subprotocol);
        sockets.push(socket);
        return socket;
      },
      term: { cols: 80, rows: 24, resize() {}, write: (_data, callback) => { if (callback) callbacks.push(callback); } },
      now: () => 0,
      captureViewport: () => 12,
      restoreViewport: (bookmark) => restored.push(bookmark),
      viewportRevision: () => revision,
    });
    session.handleRaw(INIT);
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x68])));
    revision += 1;
    callbacks.at(-1)?.();
    expect(restored).toEqual([]);
    session.shutdown();
  });

  it("does not apply a replay bookmark to live output", () => {
    const { session, sockets, restoredViewports } = harness();
    session.handleRaw(INIT);
    sockets[0].deliver(outputFrame(3n, 0, new Uint8Array([0x68])));
    expect(restoredViewports).toEqual([]);
    session.shutdown();
  });

  it("asserts size after a state snapshot", () => {
    const { session, sockets, settle } = harness();
    session.handleRaw(INIT);
    sockets[0].deliver(
      outputFrame(3n, REPLAY_ALL | TERMINAL_FLAG_REPLAY_SNAPSHOT, new Uint8Array([0x68])),
    );
    settle();
    // The PTY may hold another viewer's dimensions, and init is the point
    // where a freshly attached viewer states its own.
    expect(resizesSent(sockets[0])).toEqual([[80, 24]]);
    session.shutdown();
  });

  it("preserves a dismissed mismatch across authentication re-init and hidden Update", () => {
    const { session, sockets, posts, settle } = harness({ cols: 80, rows: 24 });
    session.handleRaw(JSON.stringify({ ...JSON.parse(BEARER_INIT), initialCols: 80, initialRows: 24 }));
    const query = new URL(sockets[0].url).searchParams;
    const owner = BigInt(query.get("viewer")!);
    const acknowledgment = BigInt(query.get("claim")!);
    sockets[0].deliver(encodeTerminalOwnership({ generation: 3n, revision: 1n, owner, acknowledgment, cols: 80, rows: 24 }));
    sockets[0].deliver(encodeTerminalResize(3n, 80, 24));
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x68])));
    settle();
    sockets[0].deliver(encodeTerminalOwnership({ generation: 3n, revision: 2n, owner: owner + 1n, acknowledgment, cols: 120, rows: 40 }));
    sockets[0].deliver(encodeTerminalResize(3n, 120, 40));
    session.handle({ type: "dismissSize" });
    session.handleRaw(BEARER_INIT);
    expect(sockets[1].url).not.toContain("cols=");
    sockets[1].deliver(encodeTerminalResize(3n, 120, 40));
    sockets[1].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x69])));
    settle();
    expect(resizesSent(sockets[1])).toEqual([]);
    expect(posts.at(-1)).not.toEqual({ type: "sizeMismatch", show: true });
    session.handle({ type: "setVisible", visible: false });
    session.handle({ type: "updateSize" });
    expect(resizesSent(sockets[1])).toEqual([]);
    session.handle({ type: "setVisible", visible: true });
    settle();
    expect(resizesSent(sockets[1])).toEqual([]);
    session.shutdown();
  });

  it("never asserts size while the host or page hides the terminal", () => {
    const { session, sockets, settle } = harness();
    session.handleRaw(INIT);
    session.handleRaw(encodeHostMessage({ type: "setVisible", visible: false }));
    sockets[0].deliver(
      outputFrame(3n, REPLAY_ALL | TERMINAL_FLAG_REPLAY_SNAPSHOT, new Uint8Array([0x68])),
    );
    settle();
    expect(resizesSent(sockets[0])).toEqual([]);
    session.handleRaw(encodeHostMessage({ type: "setVisible", visible: true }));
    session.setPageVisible(false);
    settle();
    expect(resizesSent(sockets[0])).toEqual([]);
    session.setPageVisible(true);
    settle();
    expect(resizesSent(sockets[0])).toEqual([[80, 24]]);
    session.shutdown();
  });

  it("keeps the established size without a redundant claim when returning to the foreground", () => {
    const { session, sockets, settle } = harness();
    session.handleRaw(INIT);
    sockets[0].deliver(
      outputFrame(3n, REPLAY_ALL | TERMINAL_FLAG_REPLAY_SNAPSHOT, new Uint8Array([0x68])),
    );
    settle();
    session.handleRaw(encodeHostMessage({ type: "setVisible", visible: false }));
    session.handleRaw(encodeHostMessage({ type: "setVisible", visible: true }));
    session.handleRaw(encodeHostMessage({ type: "setVisible", visible: false }));
    session.handleRaw(encodeHostMessage({ type: "setVisible", visible: true }));
    settle();
    expect(resizesSent(sockets[0])).toEqual([[80, 24]]);
    session.shutdown();
  });

  it("flags a replay that exceeds the requested cap", () => {
    const { session, sockets, posts } = harness();
    session.handleRaw(INIT);
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array(2048)));
    const replay = posts.find((msg) => msg.type === "replay");
    expect(replay && replay.type === "replay" && replay.capped).toBe(true);
    session.shutdown();
  });

  it("sends accessory bytes as input frames and honors setVisible", () => {
    const { session, sockets } = harness();
    session.handleRaw(INIT);
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array()));
    session.handleRaw(encodeHostMessage({ type: "write", dataBase64: "Gw==" }));
    const inputs = sockets[0].sent.filter((f) => new DataView(f).getUint8(0) === TERMINAL_TAG_INPUT);
    expect(inputs).toHaveLength(1);
    expect(Array.from(new Uint8Array(inputs[0], 9))).toEqual([0x1b]);
    session.handleRaw(encodeHostMessage({ type: "setVisible", visible: false }));
    session.handleRaw(encodeHostMessage({ type: "write", dataBase64: "Gw==" }));
    expect(
      sockets[0].sent.filter((f) => new DataView(f).getUint8(0) === TERMINAL_TAG_INPUT),
    ).toHaveLength(1);
    session.shutdown();
  });

  it("ignores frames from another generation", () => {
    const { session, sockets, posts, writes } = harness();
    session.handleRaw(INIT);
    sockets[0].deliver(outputFrame(2n, REPLAY_ALL, new Uint8Array([0x41])));
    expect(writes).toHaveLength(0);
    expect(posts.find((msg) => msg.type === "replay")).toBeUndefined();
    session.shutdown();
  });

  it("counts input bytes handed to the socket in the metrics message", () => {
    const { session, sockets } = harness();
    session.handleRaw(INIT);
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array()));
    session.sendInput(new TextEncoder().encode("\x1b[<0;10;5M"));
    session.sendInput(new TextEncoder().encode("\x1b[<0;10;5m"));
    const metrics = session.metricsMessage("dom");
    expect(metrics.type === "metrics" && metrics.inputBytesSent).toBe(20);
    expect(metrics.type === "metrics" && metrics.inputBytesDropped).toBe(0);
    session.shutdown();
  });

  it("carries the screen buffer and mouse tracking in the metrics message", () => {
    const { session } = harness();
    const metrics = session.metricsMessage("dom", { bufferType: "alternate", mouseTracking: "any" });
    expect(metrics.type === "metrics" && metrics.bufferType).toBe("alternate");
    expect(metrics.type === "metrics" && metrics.mouseTracking).toBe("any");
  });

  it("measures input-to-output echo latency for live frames", () => {
    const { session, sockets, tick } = harness();
    session.handleRaw(INIT);
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array()));
    tick(50);
    session.sendInput(new Uint8Array([0x6c]));
    tick(18);
    sockets[0].deliver(outputFrame(3n, 0, new Uint8Array([0x6c])));
    const metrics = session.metricsMessage("dom");
    expect(metrics.type === "metrics" && metrics.lastEchoMs).toBe(18);
    expect(metrics.type === "metrics" && metrics.firstFrameMs).toBe(0);
    session.shutdown();
  });

  it("closes the socket and reports closed on shutdown", () => {
    const { session, sockets, posts } = harness();
    session.handleRaw(INIT);
    session.handleRaw(encodeHostMessage({ type: "shutdown" }));
    expect(sockets[0].closed).toBe(true);
    expect(posts.at(-1)).toEqual({ type: "status", phase: "closed" });
  });

  it("replaces the previous socket when re-initialized", () => {
    const { session, sockets } = harness();
    session.handleRaw(INIT);
    session.handleRaw(INIT);
    expect(sockets).toHaveLength(2);
    expect(sockets[0].closed).toBe(true);
    expect(sockets[1].closed).toBe(false);
    session.shutdown();
  });

  it("holds a replay that lands while the emulator and PTY sizes disagree", () => {
    // The surface is shared by every terminal of a session, so the replay's
    // reset is the only thing that removes the previous terminal's lines.
    // Dropping it leaves them in place with the new terminal's output below.
    const { session, sockets, term, writes } = harness();
    session.handleRaw(BEARER_INIT);
    sockets[0].deliver(encodeTerminalResize(3n, 50, 20));
    // The native view laid out again between the size echo and the replay.
    term.resize(50, 30);
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x68])));
    sockets[0].deliver(outputFrame(3n, 0, new Uint8Array([0x6c])));
    expect(writes).toHaveLength(0);

    sockets[0].deliver(encodeTerminalResize(3n, 50, 30));
    expect(writes).toHaveLength(2);
    expect([...writes[0].subarray(0, 2)]).toEqual([0x1b, 0x63]);
    expect(writes[0]).toContain(0x68);
    expect([...writes[1]]).toEqual([0x6c]);
    session.shutdown();
  });

  it("follows a size another viewer set with a snapshot, not a reflow", () => {
    // Reflowing a painted screen pushes rows into scrollback that a program
    // repainting on resize then prints again, so the phone asks for a
    // snapshot at the new size and takes the size only when it arrives.
    const { session, sockets, term, writes } = harness();
    session.handleRaw(BEARER_INIT);
    sockets[0].deliver(encodeTerminalResize(3n, 80, 24));
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x68])));
    const painted = writes.length;
    expect(painted).toBeGreaterThan(0);

    sockets[0].deliver(encodeTerminalResize(3n, 120, 40));
    expect([term.cols, term.rows]).toEqual([80, 24]);
    expect(sockets[0].sent.some((f) => new DataView(f).getUint8(0) === TERMINAL_TAG_RESYNC)).toBe(true);
    sockets[0].deliver(outputFrame(3n, 0, new Uint8Array([0x6c])));
    expect(writes).toHaveLength(painted);

    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x69])));
    expect([term.cols, term.rows]).toEqual([120, 40]);
    expect(writes.at(-1)).toContain(0x69);
    session.shutdown();
  });

  it("keeps only the newest replay while frames are held", () => {
    const { session, sockets, term, writes } = harness();
    session.handleRaw(BEARER_INIT);
    sockets[0].deliver(encodeTerminalResize(3n, 50, 20));
    term.resize(50, 30);
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x68])));
    sockets[0].deliver(outputFrame(3n, 0, new Uint8Array([0x6c])));
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x69])));
    sockets[0].deliver(encodeTerminalResize(3n, 50, 30));
    expect(writes).toHaveLength(1);
    expect(writes[0]).toContain(0x69);
    session.shutdown();
  });

  it("clears the surface when it is pointed at a different terminal", () => {
    const { session, sockets, writes } = harness();
    session.handleRaw(INIT);
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x68])));
    writes.length = 0;

    session.handleRaw(encodeHostMessage({
      type: "init",
      socketBaseUrl: "wss://pm.example",
      terminalId: "8",
      generation: "1",
      ticket: "t",
      replayCapBytes: 1024,
      fontSize: 14,
    }));
    expect(writes.map((write) => [...write])).toEqual([[0x1b, 0x63]]);
    session.shutdown();
  });

  it("keeps the screen when the same terminal is re-initialized", () => {
    // A re-init for a rotated token or a retry reconnects the terminal the
    // reader is already looking at; blanking it would lose the screen for
    // as long as the reconnect takes.
    const { session, sockets, writes } = harness();
    session.handleRaw(INIT);
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x68])));
    writes.length = 0;
    session.handleRaw(INIT);
    expect(writes).toHaveLength(0);
    session.shutdown();
  });

  it("uses no ticket in the URL when accessToken is set (bearer path)", () => {
    const { session, sockets } = harness();
    session.handleRaw(BEARER_INIT);
    expect(sockets).toHaveLength(1);
    expect(sockets[0].url).not.toContain("ticket=");
    expect(sockets[0].url).toContain("generation=3");
    session.shutdown();
  });

  it("carries initial cols and rows in the first connect URL", () => {
    const { session, sockets } = harness();
    session.handleRaw(BEARER_INIT);
    expect(sockets).toHaveLength(1);
    expect(sockets[0].url).toContain("cols=50");
    expect(sockets[0].url).toContain("rows=20");
    session.shutdown();
  });

  it("omits cols and rows when initialCols/initialRows are absent", () => {
    const { session, sockets } = harness();
    session.handleRaw(INIT);
    expect(sockets).toHaveLength(1);
    expect(sockets[0].url).not.toContain("cols=");
    expect(sockets[0].url).not.toContain("rows=");
    session.shutdown();
  });

  it("bearer path reconnects carry no ticket and still omit it", () => {
    const { session, sockets } = harness();
    session.handleRaw(BEARER_INIT);
    // Simulate close + reconnect
    sockets[0].onclose?.({ code: 1006, reason: "" } as never);
    vi.advanceTimersByTime(60_000);
    expect(sockets.length).toBeGreaterThan(1);
    for (const socket of sockets) {
      expect(socket.url).not.toContain("ticket=");
    }
    session.shutdown();
  });
});

describe("TerminalViewSession replayPainted", () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  function deferredHarness() {
    const sockets: FakeSocket[] = [];
    const posts: ViewMessage[] = [];
    const pendingCallbacks: Array<() => void> = [];
    let clock = 1_000;
    const term = {
      cols: 80,
      rows: 24,
      resize(cols: number, rows: number) {
        this.cols = cols;
        this.rows = rows;
      },
      write: (_data: Uint8Array, callback?: () => void) => {
        if (callback) pendingCallbacks.push(callback);
      },
    };
    const session = new TerminalViewSession({
      post: (msg) => posts.push(msg),
      openSocket: (url, subprotocol) => {
        const socket = new FakeSocket(url, subprotocol);
        sockets.push(socket);
        return socket;
      },
      term,
      now: () => clock,
    });
    return { session, sockets, posts, pendingCallbacks };
  }

  it("posts replayPainted after replay write callback fires", () => {
    const { session, sockets, posts, pendingCallbacks } = deferredHarness();
    session.handleRaw(INIT);
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x41, 0x42])));

    // Before the write callback fires, no replayPainted
    expect(posts.some((m) => m.type === "replayPainted")).toBe(false);

    // Fire the deferred write callback
    for (const cb of pendingCallbacks) cb();

    expect(posts.some((m) => m.type === "replayPainted")).toBe(true);
    session.shutdown();
  });

  it("does not post replayPainted for live frames", () => {
    const { session, sockets, posts, pendingCallbacks } = deferredHarness();
    session.handleRaw(INIT);
    // Complete replay first so live frames are accepted
    sockets[0].deliver(outputFrame(3n, REPLAY_ALL, new Uint8Array([0x41])));
    for (const cb of pendingCallbacks) cb();
    posts.length = 0;
    pendingCallbacks.length = 0;

    // Deliver a live frame
    sockets[0].deliver(outputFrame(3n, 0, new Uint8Array([0x42])));
    for (const cb of pendingCallbacks) cb();

    expect(posts.some((m) => m.type === "replayPainted")).toBe(false);
    session.shutdown();
  });
});
