import { describe, expect, it } from "vitest";
import { PtyBuffer, terminalSizeChanged, type PtyFrame } from "./pty";

function frame(byte: number, replay = false): PtyFrame {
  return { data: new Uint8Array([byte]), replay };
}

function collector() {
  const frames: PtyFrame[] = [];
  return { frames, sink: (f: PtyFrame) => frames.push(f) };
}

describe("PtyBuffer", () => {
  it("flushes frames received before the sink connects, in order", () => {
    const buffer = new PtyBuffer();
    buffer.push(frame(1, true));
    buffer.push(frame(2));
    buffer.push(frame(3));

    const { frames, sink } = collector();
    buffer.connect(sink);

    expect(frames.map((f) => f.data[0])).toEqual([1, 2, 3]);
    expect(frames.map((f) => f.replay)).toEqual([true, false, false]);
  });

  it("delivers frames directly once connected", () => {
    const buffer = new PtyBuffer();
    const { frames, sink } = collector();
    buffer.connect(sink);
    buffer.push(frame(7));
    expect(frames).toHaveLength(1);
    expect(frames[0].data[0]).toBe(7);
  });

  it("does not replay already-delivered frames on reconnect", () => {
    const buffer = new PtyBuffer();
    const first = collector();
    buffer.connect(first.sink);
    buffer.push(frame(1));

    buffer.disconnect();
    buffer.push(frame(2));

    const second = collector();
    buffer.connect(second.sink);
    buffer.push(frame(3));

    expect(first.frames.map((f) => f.data[0])).toEqual([1]);
    expect(second.frames.map((f) => f.data[0])).toEqual([2, 3]);
  });

  it("drops held frames on clear", () => {
    const buffer = new PtyBuffer();
    buffer.push(frame(1));
    buffer.clear();
    const { frames, sink } = collector();
    buffer.connect(sink);
    expect(frames).toHaveLength(0);
  });
});

describe("terminal size tracking", () => {
  it("sends the initial size and suppresses unchanged repeats", () => {
    expect(terminalSizeChanged(null, 120, 40)).toBe(true);
    expect(terminalSizeChanged({ cols: 120, rows: 40 }, 120, 40)).toBe(false);
    expect(terminalSizeChanged({ cols: 120, rows: 40 }, 121, 40)).toBe(true);
    expect(terminalSizeChanged({ cols: 120, rows: 40 }, 120, 41)).toBe(true);
  });
});
