import { encodeTerminalOwnership } from "./terminalFrame";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SocketCloseEvent, SocketConnector, SocketMessageEvent } from "../platform";
import { createTerminalViewerIdentity, MAX_PENDING_INPUT_BYTES, TerminalSocket, type TerminalStreamStatus } from "./terminalSocket";
import {
  encodeTerminalResize,
  TERMINAL_FLAG_REPLAY,
  TERMINAL_FLAG_REPLAY_END,
  TERMINAL_FLAG_REPLAY_SNAPSHOT,
  TERMINAL_FLAG_REPLAY_START,
  TERMINAL_TAG_OUTPUT,
  TERMINAL_TAG_RESIZE,
  TERMINAL_TAG_INPUT,
  TERMINAL_TAG_INPUT_SUBMIT,
  TERMINAL_TAG_RESYNC,
} from "./terminalFrame";

class FakeWebSocket {
  static readonly OPEN = 1;
  static instances: FakeWebSocket[] = [];
  readyState = FakeWebSocket.OPEN;
  binaryType = "";
  sent: ArrayBuffer[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: SocketMessageEvent) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: ((event: SocketCloseEvent) => void) | null = null;

  constructor(public url: string, public protocol?: string) {
    FakeWebSocket.instances.push(this);
  }

  send(data: ArrayBuffer): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = 3;
  }

  output(generation: bigint, flags: number, data: number[] = []): void {
    const frame = new Uint8Array(10 + data.length);
    const view = new DataView(frame.buffer);
    view.setUint8(0, TERMINAL_TAG_OUTPUT);
    view.setBigUint64(1, generation, true);
    view.setUint8(9, flags);
    frame.set(data, 10);
    this.onmessage?.({ data: frame.buffer });
  }

  resizeEcho(generation: bigint, cols: number, rows: number): void {
    this.onmessage?.({ data: encodeTerminalResize(generation, cols, rows) });
  }

  disconnect(reason = "relay unavailable"): void {
    this.readyState = 3;
    this.onclose?.({ code: 1006, reason });
  }
}

const connect: SocketConnector = (path, subprotocol) => new FakeWebSocket(path, subprotocol);

describe("TerminalSocket", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.spyOn(Math, "random").mockReturnValue(0.5);
    FakeWebSocket.instances = [];
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("buffers replay before painting and sends the latest resize after it ends", () => {
    const frames: Array<{ data: number[]; replay: boolean }> = [];
    const socket = new TerminalSocket(connect, 7n, 3n, (frame) => frames.push({
      data: [...frame.data],
      replay: frame.replay,
    }), () => {});
    const wire = FakeWebSocket.instances[0];
    socket.resize(120, 40);
    socket.input(new Uint8Array([1]));
    expect(wire.sent).toHaveLength(0);

    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START, [2]);
    wire.output(3n, TERMINAL_FLAG_REPLAY, [3]);
    socket.input(new Uint8Array([3]));
    expect(frames).toEqual([]);
    expect(wire.sent).toHaveLength(0);
    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_END, [4]);

    expect(frames).toEqual([{ data: [2, 3, 4], replay: true }]);
    // The size the PTY was told about must precede the input it applies to.
    expect(new DataView(wire.sent[0]).getUint8(0)).toBe(TERMINAL_TAG_RESIZE);
    expect(wire.sent.slice(1).map((frame) => [...new Uint8Array(frame).slice(9)])).toEqual([[1], [3]]);
    socket.input(new Uint8Array([5]));
    expect(wire.sent).toHaveLength(4);
    socket.input(new Uint8Array([13]), true);
    expect(new DataView(wire.sent[4]).getUint8(0)).toBe(TERMINAL_TAG_INPUT_SUBMIT);
    wire.output(3n, 0, [6]);
    expect(frames.at(-1)).toEqual({ data: [6], replay: false });
    socket.close();
  });

  it("tracks the daemon's PTY size echo and drops stale generations", () => {
    const frames: number[][] = [];
    const socket = new TerminalSocket(connect, 7n, 3n, (frame) => frames.push([...frame.data]), () => {});
    const wire = FakeWebSocket.instances[0];
    const sizes: Array<{ cols: number; rows: number }> = [];
    const unsubscribe = socket.onPtySize((size) => sizes.push(size));
    expect(socket.ptySize()).toBeNull();

    // The attach echo arrives before replay and never reaches the sink.
    wire.resizeEcho(3n, 100, 30);
    expect(socket.ptySize()).toEqual({ cols: 100, rows: 30 });
    expect(sizes).toEqual([{ cols: 100, rows: 30 }]);
    expect(socket.stats().ptySize).toEqual({ cols: 100, rows: 30 });
    expect(frames).toEqual([]);

    wire.resizeEcho(2n, 55, 5);
    expect(socket.ptySize()).toEqual({ cols: 100, rows: 30 });
    expect(sizes).toHaveLength(1);

    unsubscribe();
    wire.resizeEcho(3n, 90, 25);
    expect(sizes).toHaveLength(1);
    expect(socket.ptySize()).toEqual({ cols: 90, rows: 25 });
    socket.close();
  });

  it("delivers input typed before the stream is ready, in order, once it comes online", () => {
    const socket = new TerminalSocket(connect, 7n, 3n, () => {}, () => {});
    const wire = FakeWebSocket.instances[0];
    socket.input(new Uint8Array([110, 101, 101, 100, 115]));
    socket.input(new Uint8Array([13]), true);
    expect(wire.sent).toHaveLength(0);
    expect(socket.stats()).toMatchObject({ inputBytesSent: 0, inputBytesPending: 6, inputBytesDropped: 0 });

    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END, [1]);

    expect(wire.sent.map((frame) => [...new Uint8Array(frame).slice(9)])).toEqual([
      [110, 101, 101, 100, 115],
      [13],
    ]);
    expect(new DataView(wire.sent[0]).getUint8(0)).toBe(TERMINAL_TAG_INPUT);
    expect(new DataView(wire.sent[1]).getUint8(0)).toBe(TERMINAL_TAG_INPUT_SUBMIT);
    expect(socket.stats()).toMatchObject({ inputBytesSent: 6, inputBytesPending: 0, inputBytesDropped: 0 });
    socket.close();
  });

  it("does not re-send a delivered size when it reconnects", () => {
    const socket = new TerminalSocket(connect, 7n, 3n, () => {}, () => {});
    const first = FakeWebSocket.instances[0];
    first.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END);
    socket.resize(168, 48);
    expect(first.sent.map((frame) => new DataView(frame).getUint8(0))).toEqual([TERMINAL_TAG_RESIZE]);
    first.disconnect();

    vi.advanceTimersByTime(1_000);
    const reconnected = FakeWebSocket.instances.at(-1)!;
    reconnected.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END);
    expect(reconnected.sent).toHaveLength(0);

    // A size offered while the stream was down is delivered exactly once.
    reconnected.disconnect();
    socket.resize(56, 30);
    vi.advanceTimersByTime(1_000);
    const again = FakeWebSocket.instances.at(-1)!;
    again.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END);
    expect(again.sent.map((frame) => new DataView(frame).getUint16(9, true))).toEqual([56]);
    again.disconnect();
    vi.advanceTimersByTime(2_000);
    const last = FakeWebSocket.instances.at(-1)!;
    last.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END);
    expect(last.sent).toHaveLength(0);
  });

  it("suppresses redundant resizes when the daemon already echoed that size", () => {
    const socket = new TerminalSocket(connect, 7n, 3n, () => {}, () => {});
    const wire = FakeWebSocket.instances[0];
    wire.resizeEcho(3n, 120, 40);
    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END);
    socket.resize(120, 40);
    expect(wire.sent).toHaveLength(0);

    socket.resize(120, 41);
    expect(wire.sent).toHaveLength(1);
    socket.close();
  });

  it("holds input across a reconnect and abandons it when the terminal restarts", () => {
    const socket = new TerminalSocket(connect, 7n, 3n, () => {}, () => {});
    FakeWebSocket.instances[0].output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END);
    FakeWebSocket.instances[0].disconnect();
    socket.input(new Uint8Array([97]));
    expect(socket.stats().inputBytesPending).toBe(1);

    vi.advanceTimersByTime(1_000);
    const reconnected = FakeWebSocket.instances.at(-1)!;
    reconnected.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END);
    expect(reconnected.sent.map((frame) => [...new Uint8Array(frame).slice(9)])).toEqual([[97]]);

    // A new generation is a different process, so bytes aimed at the old one
    // must not be replayed into it.
    reconnected.disconnect();
    socket.input(new Uint8Array([98]));
    expect(socket.stats().inputBytesPending).toBe(1);
    socket.updateGeneration(4n);
    const restarted = FakeWebSocket.instances.at(-1)!;
    restarted.output(4n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END);
    expect(restarted.sent).toHaveLength(0);
    expect(socket.stats()).toMatchObject({ inputBytesPending: 0, inputBytesDropped: 1 });
    socket.close();
  });

  it("stops holding input once the pending buffer is full", () => {
    const socket = new TerminalSocket(connect, 7n, 3n, () => {}, () => {});
    socket.input(new Uint8Array(MAX_PENDING_INPUT_BYTES));
    socket.input(new Uint8Array([1]));
    expect(socket.stats()).toMatchObject({
      inputBytesPending: MAX_PENDING_INPUT_BYTES,
      inputBytesDropped: 1,
    });
    socket.close();
  });

  it("marks snapshot replays for the sink and stats, and resync refetches", () => {
    const frames: Array<{ replay: boolean; snapshot?: boolean }> = [];
    const socket = new TerminalSocket(connect, 7n, 3n, (frame) => frames.push({
      replay: frame.replay,
      snapshot: frame.snapshot,
    }), () => {});
    const wire = FakeWebSocket.instances[0];
    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_SNAPSHOT, [1]);
    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_END | TERMINAL_FLAG_REPLAY_SNAPSHOT, [2]);
    expect(frames).toEqual([{ replay: true, snapshot: true }]);
    expect(socket.stats().lastReplaySnapshot).toBe(true);

    socket.resync();
    const rewire = FakeWebSocket.instances.at(-1)!;
    expect(rewire).not.toBe(wire);
    rewire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END, [3]);
    expect(frames.at(-1)).toEqual({ replay: true, snapshot: false });
    expect(socket.stats().lastReplaySnapshot).toBe(false);
    expect(socket.stats().socketsOpened).toBe(2);
    socket.close();
  });

  it("queues a snapshot until the first replay finishes without reconnecting", () => {
    const frames: Array<{ replay: boolean }> = [];
    const socket = new TerminalSocket(connect, 7n, 3n, (frame) => frames.push({ replay: frame.replay }), () => {});
    socket.refreshSnapshot();
    socket.refreshSnapshot();
    expect(FakeWebSocket.instances).toHaveLength(1);
    const wire = FakeWebSocket.instances[0];
    expect(wire.sent).toHaveLength(0);
    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END, [1]);

    expect(wire.sent).toHaveLength(1);
    expect(FakeWebSocket.instances).toHaveLength(1);
    const resync = wire.sent.at(-1)!;
    expect(new DataView(resync).getUint8(0)).toBe(TERMINAL_TAG_RESYNC);
    expect(new DataView(resync).getBigUint64(1, true)).toBe(3n);
    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END, [2]);
    expect(frames.filter((frame) => frame.replay)).toHaveLength(2);
    expect(socket.stats().socketsOpened).toBe(1);
    socket.close();
  });

  it("keeps a connecting socket while a snapshot request waits for replay", () => {
    const socket = new TerminalSocket(connect, 7n, 3n, () => {}, () => {});
    const wire = FakeWebSocket.instances[0];
    wire.readyState = 0;
    socket.refreshSnapshot();
    expect(FakeWebSocket.instances).toHaveLength(1);
    expect(wire.sent).toHaveLength(0);
    wire.readyState = FakeWebSocket.OPEN;
    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END, [1]);
    expect(wire.sent).toHaveLength(1);
    expect(new DataView(wire.sent[0]).getUint8(0)).toBe(TERMINAL_TAG_RESYNC);
    socket.close();
  });

  it("accounts replay, output, input, holds, and resizes in its stats", () => {
    const socket = new TerminalSocket(connect, 7n, 3n, () => {}, () => {});
    const wire = FakeWebSocket.instances[0];
    socket.input(new Uint8Array([1, 2, 3]));
    socket.resize(120, 40);
    expect(socket.stats()).toMatchObject({
      generation: "3",
      phase: "reconnecting",
      socketsOpened: 1,
      socketsClosed: 0,
      outputBytes: 0,
      replayCount: 0,
      inputBytesSent: 0,
      inputBytesPending: 3,
      inputBytesDropped: 0,
      resizesSent: [],
    });

    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START, [2, 2]);
    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_END, [4]);
    socket.input(new Uint8Array([5, 6]));
    wire.output(3n, 0, [7, 8, 9]);
    const stats = socket.stats();
    expect(stats).toMatchObject({
      phase: "online",
      outputBytes: 6,
      replayCount: 1,
      lastReplayBytes: 3,
      inputBytesSent: 5,
      inputBytesPending: 0,
      inputBytesDropped: 0,
    });
    expect(stats.resizesSent.map((entry) => `${entry.cols}x${entry.rows}`)).toEqual(["120x40"]);
    expect(stats.lastOutputAt).not.toBeNull();
    expect(stats.lastReplayAt).not.toBeNull();

    wire.disconnect();
    expect(socket.stats().socketsClosed).toBe(1);
    expect(socket.stats().phase).toBe("reconnecting");
    socket.close();
  });

  it("reconnects a new generation through the existing output sink", () => {
    const frames: Array<{ data: number[]; replay: boolean }> = [];
    const socket = new TerminalSocket(connect, 8n, 1n, (frame) => frames.push({
      data: [...frame.data],
      replay: frame.replay,
    }), () => {});
    FakeWebSocket.instances[0].output(
      1n,
      TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END,
      [1],
    );

    socket.updateGeneration(2n);
    expect(FakeWebSocket.instances).toHaveLength(2);
    FakeWebSocket.instances[1].output(
      2n,
      TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END,
      [2],
    );

    expect(frames).toEqual([
      { data: [1], replay: true },
      { data: [2], replay: true },
    ]);
    socket.close();
  });

  it("keeps reconnects quiet for 750 ms and offers retry after 10 seconds", () => {
    const statuses: TerminalStreamStatus[] = [];
    const socket = new TerminalSocket(connect, 9n, 1n, () => {}, () => {});
    socket.subscribe((status) => statuses.push(status));
    FakeWebSocket.instances[0].disconnect();

    vi.advanceTimersByTime(749);
    expect(statuses).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(statuses.at(-1)).toEqual({ phase: "reconnecting", canRetry: false });
    vi.advanceTimersByTime(9_250);
    expect(statuses.at(-1)).toEqual({
      phase: "reconnecting",
      canRetry: true,
      lastError: "relay unavailable",
    });
    expect(FakeWebSocket.instances.length).toBeGreaterThan(1);
    socket.close();
  });

  it("cancels a pending reconnect when the terminal handle closes", () => {
    const socket = new TerminalSocket(connect, 10n, 1n, () => {}, () => {});
    FakeWebSocket.instances[0].disconnect();
    expect(FakeWebSocket.instances).toHaveLength(1);

    socket.close();
    vi.advanceTimersByTime(30_000);

    expect(FakeWebSocket.instances).toHaveLength(1);
  });

  it("parks while its worker is offline and reconnects once when it returns", () => {
    const statuses: TerminalStreamStatus[] = [];
    const socket = new TerminalSocket(connect, 11n, 1n, () => {}, () => {});
    socket.subscribe((status) => statuses.push(status));

    socket.setAvailable(false, "worker remote is offline");
    expect(statuses.at(-1)).toEqual({
      phase: "reconnecting",
      canRetry: false,
      lastError: "worker remote is offline",
    });
    expect(FakeWebSocket.instances).toHaveLength(1);
    vi.advanceTimersByTime(30_000);
    expect(FakeWebSocket.instances).toHaveLength(1);

    socket.updateGeneration(2n);
    socket.setAvailable(true);
    expect(FakeWebSocket.instances).toHaveLength(2);
    expect(FakeWebSocket.instances[1].url).toContain("generation=2");
    vi.advanceTimersByTime(30_000);
    expect(FakeWebSocket.instances).toHaveLength(2);
    socket.close();
  });

  it("flushes a pending DOM resize after replay completes", () => {
    const events: string[] = [];
    const socket = new TerminalSocket(connect, 7n, 3n, (frame) => {
      events.push(frame.replay ? "sink" : "live");
      expect(FakeWebSocket.instances[0].sent).toHaveLength(0);
    }, () => {});
    const wire = FakeWebSocket.instances[0];
    socket.resize(142, 48);
    wire.resizeEcho(3n, 120, 32);
    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_SNAPSHOT, [1]);
    wire.output(3n, 0, [9]);
    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_END | TERMINAL_FLAG_REPLAY_SNAPSHOT, [2]);
    expect(events).toEqual(["sink", "live"]);
    expect(wire.sent).toHaveLength(1);
    socket.close();
  });

  it("delivers live bytes that arrived during replay after the snapshot, in order", () => {
    const frames: Array<{ data: number[]; replay: boolean }> = [];
    const order: string[] = [];
    const socket = new TerminalSocket(connect, 7n, 3n, (frame) => {
      frames.push({ data: [...frame.data], replay: frame.replay });
      order.push(frame.replay ? "snapshot" : "live");
    }, () => {});
    const wire = FakeWebSocket.instances[0];
    socket.onPtySize(() => order.push("echo"));

    wire.resizeEcho(3n, 100, 30);
    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_SNAPSHOT, [1]);
    wire.output(3n, 0, [9]);
    wire.output(3n, 0, [8]);
    expect(frames).toEqual([]);
    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_END | TERMINAL_FLAG_REPLAY_SNAPSHOT, [2]);

    expect(frames).toEqual([
      { data: [1, 2], replay: true },
      { data: [9], replay: false },
      { data: [8], replay: false },
    ]);
    expect(order).toEqual(["echo", "snapshot", "live", "live"]);
    expect(socket.ptySize()).toEqual({ cols: 100, rows: 30 });
    socket.close();
  });

  it("applies the size echo before the sink sees replay", () => {
    const events: string[] = [];
    const socket = new TerminalSocket(connect, 7n, 3n, () => events.push("sink"), () => {});
    const wire = FakeWebSocket.instances[0];
    socket.onPtySize((size) => events.push(`echo:${size.cols}x${size.rows}`));
    wire.resizeEcho(3n, 91, 33);
    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END, [1]);
    expect(events).toEqual(["echo:91x33", "sink"]);
    socket.close();
  });

  it("includes initial cols and rows in websocket connect URL when provided", () => {
    const socket = new TerminalSocket(
      connect,
      12n,
      1n,
      () => {},
      () => {},
      null,
      { cols: 165, rows: 45 },
    );
    expect(FakeWebSocket.instances).toHaveLength(1);
    const query = new URLSearchParams(FakeWebSocket.instances[0].url.split("?")[1]);
    expect(query.get("generation")).toBe("1");
    expect(query.get("cols")).toBe("165");
    expect(query.get("rows")).toBe("45");
    expect(BigInt(query.get("viewer")!)).toBeGreaterThan(0n);
    expect(query.get("claim")).toBe("1");
    socket.close();
  });

  it("does not reclaim the initial size through a reconnect URL", () => {
    const socket = new TerminalSocket(connect, 12n, 1n, () => {}, () => {}, null, { cols: 165, rows: 45 });
    const first = FakeWebSocket.instances[0];
    first.output(1n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END);
    first.resizeEcho(1n, 50, 24);
    first.disconnect();
    vi.advanceTimersByTime(1_000);
    const query = new URLSearchParams(FakeWebSocket.instances.at(-1)!.url.split("?")[1]);
    expect(query.get("generation")).toBe("1");
    expect(query.has("cols")).toBe(false);
    expect(query.has("rows")).toBe(false);
    expect(query.has("claim")).toBe(false);
    socket.close();
  });
  it("orders ownership independently of delayed local size echoes", () => {
    const socket = new TerminalSocket(connect, 7n, 3n, () => {}, () => {}, null, { cols: 120, rows: 40 });
    const wire = FakeWebSocket.instances[0];
    const query = new URLSearchParams(wire.url.split("?")[1]);
    const viewer = BigInt(query.get("viewer")!);
    const initial = BigInt(query.get("claim")!);
    const states: boolean[] = [];
    socket.onViewerOwnership((state) => states.push(state.local));
    const owner = (revision: bigint, id: bigint, acknowledgment: bigint) => wire.onmessage?.({ data: encodeTerminalOwnership({ generation: 3n, revision, owner: id, acknowledgment, cols: 80, rows: 24 }) });
    owner(1n, viewer, initial);
    wire.output(3n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_START | TERMINAL_FLAG_REPLAY_END);
    socket.resize(100, 30);
    socket.resize(120, 40);
    owner(2n, viewer, initial + 1n);
    owner(3n, viewer + 1n, initial + 1n);
    expect(states).toEqual([true]);
    owner(4n, viewer, initial + 2n);
    wire.resizeEcho(3n, 100, 30);
    owner(3n, viewer + 1n, initial + 1n);
    expect(states).toEqual([true, true]);
    owner(5n, viewer + 1n, initial + 2n);
    expect(states).toEqual([true, true, false]);
    socket.close();
  });

  it("accepts a coalesced remote owner after its own latest claim was acknowledged", () => {
    const socket = new TerminalSocket(connect, 7n, 3n, () => {}, () => {}, null, { cols: 120, rows: 40 });
    const wire = FakeWebSocket.instances[0];
    const query = new URLSearchParams(wire.url.split("?")[1]);
    const viewer = BigInt(query.get("viewer")!);
    const states: boolean[] = [];
    socket.onViewerOwnership((state) => states.push(state.local));
    wire.onmessage?.({ data: encodeTerminalOwnership({ generation: 3n, revision: 2n, owner: viewer + 1n, acknowledgment: BigInt(query.get("claim")!), cols: 80, rows: 24 }) });
    expect(states).toEqual([false]);
    socket.close();
  });

});


describe("terminal viewer identity", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("uses the available crypto provider with its receiver", () => {
    const crypto = {
      getRandomValues(values: Uint32Array): Uint32Array {
        expect(this).toBe(crypto);
        values.set([1, 2]);
        return values;
      },
    };
    vi.stubGlobal("crypto", crypto);
    expect(createTerminalViewerIdentity()).toEqual({ id: 4294967299n, request: 0n });
  });

  it("creates a nonzero identity when crypto is unavailable", () => {
    vi.stubGlobal("crypto", undefined);
    const identity = createTerminalViewerIdentity();
    expect(identity.id).toBeGreaterThan(0n);
    expect(identity.request).toBe(0n);
  });
});
