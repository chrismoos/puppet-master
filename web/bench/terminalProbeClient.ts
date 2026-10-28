import type { PmClient, PtyHandle } from "@puppet-master/client-core/ws/client";
import type { PtySink, TerminalSize } from "@puppet-master/client-core/ws/pty";
import type { TerminalSocketStats } from "@puppet-master/client-core/ws/terminalSocket";

const DEFAULT_COLS = 80;
const DEFAULT_ROWS = 24;
const RESET_SEQUENCE = new TextEncoder().encode("\x1bc");

export function createTerminalProbeClient(initialOutput: (key: string) => Uint8Array = () => new Uint8Array()) {
  const sinks = new Map<string, PtySink>();
  const outputs = new Map<string, Uint8Array>();
  const inputs = new Map<string, number>();
  const opens = new Map<string, number>();
  const closes = new Map<string, number>();
  const resizes = new Map<string, string[]>();
  const socketStats = new Map<string, TerminalSocketStats>();

  const snapshot = (key: string) => {
    const output = outputs.get(key) ?? initialOutput(key);
    outputs.set(key, output);
    const data = new Uint8Array(RESET_SEQUENCE.length + output.length);
    data.set(RESET_SEQUENCE);
    data.set(output, RESET_SEQUENCE.length);
    queueMicrotask(() => {
      const sink = sinks.get(key);
      if (!sink) return;
      const stats = socketStats.get(key)!;
      stats.outputBytes += data.byteLength;
      stats.lastOutputAt = performance.now();
      stats.replayCount += 1;
      stats.lastReplayAt = performance.now();
      stats.lastReplayBytes = data.byteLength;
      stats.lastReplaySnapshot = true;
      sink({ data, replay: true, snapshot: true });
    });
  };

  const handle = (key: string, initialSize?: TerminalSize | null): PtyHandle => {
    const socketsOpened = (opens.get(key) ?? 0) + 1;
    opens.set(key, socketsOpened);
    let size = initialSize ?? { cols: DEFAULT_COLS, rows: DEFAULT_ROWS };
    const sizeListeners = new Set<(value: TerminalSize) => void>();
    const stats: TerminalSocketStats = {
      generation: key,
      phase: "online",
      socketsOpened,
      socketsClosed: 0,
      outputBytes: 0,
      lastOutputAt: null,
      replayCount: 0,
      lastReplayBytes: 0,
      lastReplayAt: null,
      inputBytesSent: 0,
      inputBytesPending: 0,
      inputBytesDropped: 0,
      resizesSent: [],
      ptySize: size,
      lastReplaySnapshot: true,
      lastError: "",
    };
    socketStats.set(key, stats);
    return {
      connect: (sink) => { sinks.set(key, sink); snapshot(key); },
      input: (data) => {
        inputs.set(key, (inputs.get(key) ?? 0) + data.byteLength);
        stats.inputBytesSent += data.byteLength;
      },
      ack: () => {},
      resize: (cols, rows) => {
        size = { cols, rows };
        stats.ptySize = size;
        stats.resizesSent.push({ cols, rows, at: performance.now() });
        resizes.set(key, [...resizes.get(key) ?? [], `${cols}x${rows}`]);
        queueMicrotask(() => { for (const listener of sizeListeners) listener(size); });
      },
      onStatus: (listener) => { listener({ phase: "online" }); return () => {}; },
      ptySize: () => size,
      onPtySize: (listener) => { sizeListeners.add(listener); return () => sizeListeners.delete(listener); },
      stats: () => stats,
      resync: () => snapshot(key),
      refreshSnapshot: () => snapshot(key),
      requestWholeHistory: () => {},
      retry: () => {},
      close: () => {
        sinks.delete(key);
        closes.set(key, (closes.get(key) ?? 0) + 1);
        stats.socketsClosed += 1;
      },
    };
  };

  const client = {
    openPty: (id: bigint, size?: TerminalSize | null) => handle(`s:${id}`, size),
    openTerminal: (id: bigint, size?: TerminalSize | null) => handle(`t:${id}`, size),
  } as unknown as PmClient;

  const write = (key: string, data: Uint8Array, replay = false) => {
    if (replay) {
      outputs.set(key, data);
      snapshot(key);
      return;
    }
    const previous = outputs.get(key) ?? new Uint8Array();
    const next = new Uint8Array(previous.length + data.length);
    next.set(previous);
    next.set(data, previous.length);
    outputs.set(key, next);
    const sink = sinks.get(key);
    if (!sink) return;
    const stats = socketStats.get(key)!;
    stats.outputBytes += data.byteLength;
    stats.lastOutputAt = performance.now();
    stats.lastReplaySnapshot = false;
    sink({ data, replay: false });
  };

  return { client, inputs, opens, closes, resizes, write };
}
