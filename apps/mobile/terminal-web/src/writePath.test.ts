import { describe, expect, it } from "vitest";

import { connectTerminalOutput, type TerminalInputSink } from "./writePath";

const SGR_PRESS = "\x1b[<0;10;5M";
const SGR_RELEASE = "\x1b[<0;10;5m";
const SGR_MOTION_BATCH = "\x1b[<32;1;1M\x1b[<32;2;1M\x1b[<32;3;1M";
const X10_PRESS = "\x1b[M !!";

function harness() {
  let emitData: (data: string) => void = () => {};
  let emitBinary: (data: string) => void = () => {};
  const sent: { bytes: number[]; submitted: boolean }[] = [];
  const sink: TerminalInputSink = {
    sendInput: (bytes, submitted = false) => sent.push({ bytes: Array.from(bytes), submitted }),
  };
  connectTerminalOutput(
    {
      onData: (listener) => {
        emitData = listener;
      },
      onBinary: (listener) => {
        emitBinary = listener;
      },
    },
    sink,
  );
  return { emitData: (d: string) => emitData(d), emitBinary: (d: string) => emitBinary(d), sent };
}

function bytesOf(text: string): number[] {
  return Array.from(new TextEncoder().encode(text));
}

describe("connectTerminalOutput", () => {
  it("delivers every mouse report chunk to the socket byte-for-byte", () => {
    const { emitData, sent } = harness();
    emitData(SGR_PRESS);
    emitData(SGR_MOTION_BATCH);
    emitData(SGR_RELEASE);
    expect(sent.map((entry) => entry.bytes)).toEqual([
      bytesOf(SGR_PRESS),
      bytesOf(SGR_MOTION_BATCH),
      bytesOf(SGR_RELEASE),
    ]);
  });

  it("delivers binary mouse reports without UTF-8 mangling", () => {
    const { emitBinary, sent } = harness();
    const raw = `\x1b[M\x20\xff\xff`;
    emitBinary(raw);
    expect(sent).toEqual([{ bytes: [0x1b, 0x5b, 0x4d, 0x20, 0xff, 0xff], submitted: false }]);
  });

  it("delivers keystrokes and marks a lone carriage return submitted", () => {
    const { emitData, sent } = harness();
    emitData("a");
    emitData("\r");
    expect(sent).toEqual([
      { bytes: bytesOf("a"), submitted: false },
      { bytes: bytesOf("\r"), submitted: true },
    ]);
  });

  it("passes X10 reports through onData unfiltered", () => {
    const { emitData, sent } = harness();
    emitData(X10_PRESS);
    expect(sent.map((entry) => entry.bytes)).toEqual([bytesOf(X10_PRESS)]);
  });
});
