import { decodeTerminalOwnership, encodeTerminalOwnership, encodeTerminalResizeRequest, TERMINAL_TAG_OWNERSHIP, TERMINAL_TAG_RESIZE_REQUEST } from "./terminalFrame";
import { describe, expect, it } from "vitest";
import {
  decodeTerminalOutput,
  decodeTerminalResize,
  encodeTerminalAck,
  encodeTerminalResync,
  encodeTerminalInput,
  encodeTerminalResize,
  TERMINAL_TAG_ACK,
  TERMINAL_TAG_RESYNC,
  TERMINAL_FLAG_REPLAY,
  TERMINAL_FLAG_REPLAY_END,
  TERMINAL_TAG_INPUT,
  TERMINAL_TAG_INPUT_SUBMIT,
  TERMINAL_TAG_RESIZE,
} from "./terminalFrame";

function output(generation: bigint, flags: number, data: number[]): ArrayBuffer {
  const frame = new Uint8Array(10 + data.length);
  const view = new DataView(frame.buffer);
  view.setUint8(0, 1);
  view.setBigUint64(1, generation, true);
  view.setUint8(9, flags);
  frame.set(data, 10);
  return frame.buffer;
}

describe("terminal frames", () => {
  it("decodes output generation, flags, and bytes", () => {
    expect(decodeTerminalOutput(output(12n, TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_END, [4, 5])))
      .toEqual({
        generation: 12n,
        flags: TERMINAL_FLAG_REPLAY | TERMINAL_FLAG_REPLAY_END,
        data: new Uint8Array([4, 5]),
      });
  });

  it("encodes input and resize in little endian", () => {
    const input = new DataView(encodeTerminalInput(0x0102n, new Uint8Array([9])));
    expect(input.getUint8(0)).toBe(TERMINAL_TAG_INPUT);
    expect(input.getBigUint64(1, true)).toBe(0x0102n);
    expect(input.getUint8(9)).toBe(9);

    const submit = new DataView(encodeTerminalInput(0x0102n, new Uint8Array([13]), true));
    expect(submit.getUint8(0)).toBe(TERMINAL_TAG_INPUT_SUBMIT);
    expect(submit.getUint8(9)).toBe(13);

    const resize = new DataView(encodeTerminalResize(8n, 120, 40));
    expect(resize.getUint8(0)).toBe(TERMINAL_TAG_RESIZE);
    expect(resize.getBigUint64(1, true)).toBe(8n);
    expect(resize.getUint16(9, true)).toBe(120);
    expect(resize.getUint16(11, true)).toBe(40);
  });

  it("rejects malformed and non-output frames", () => {
    expect(decodeTerminalOutput(new ArrayBuffer(9))).toBeNull();
    expect(decodeTerminalOutput(encodeTerminalInput(1n, new Uint8Array()))).toBeNull();
    expect(decodeTerminalOutput(encodeTerminalResize(1n, 80, 24))).toBeNull();
  });

  it("round-trips a resize echo", () => {
    expect(decodeTerminalResize(encodeTerminalResize(7n, 132, 43))).toEqual({
      generation: 7n,
      cols: 132,
      rows: 43,
    });
  });

  it("rejects malformed and non-resize frames as resize echoes", () => {
    expect(decodeTerminalResize(new ArrayBuffer(12))).toBeNull();
    expect(decodeTerminalResize(new ArrayBuffer(14))).toBeNull();
    expect(decodeTerminalResize(encodeTerminalInput(1n, new Uint8Array(4)))).toBeNull();
    expect(decodeTerminalResize(output(1n, 0, [0, 0, 0]))).toBeNull();
  });

  it("encodes an ack as the tag, the generation and a byte count", () => {
    const frame = encodeTerminalAck(5n, 70000);
    expect(frame.byteLength).toBe(13);
    const view = new DataView(frame);
    expect(view.getUint8(0)).toBe(TERMINAL_TAG_ACK);
    expect(view.getBigUint64(1, true)).toBe(5n);
    expect(view.getUint32(9, true)).toBe(70000);
  });

  it("encodes a resync as the tag and the generation", () => {
    const frame = encodeTerminalResync(9n);
    expect(frame.byteLength).toBe(9);
    const view = new DataView(frame);
    expect(view.getUint8(0)).toBe(TERMINAL_TAG_RESYNC);
    expect(view.getBigUint64(1, true)).toBe(9n);
  });
  it("round-trips ordered ownership and rejects malformed ownership frames", () => {
    const state = { generation: 3n, revision: 7n, owner: 11n, acknowledgment: 5n, cols: 120, rows: 40 };
    const frame = encodeTerminalOwnership(state);
    expect(new Uint8Array(frame)[0]).toBe(TERMINAL_TAG_OWNERSHIP);
    expect(decodeTerminalOwnership(frame)).toEqual(state);
    expect(decodeTerminalOwnership(frame.slice(0, -1))).toBeNull();
    expect(decodeTerminalOwnership(encodeTerminalResizeRequest(3n, 5n, 120, 40))).toBeNull();
    expect(new Uint8Array(encodeTerminalResizeRequest(3n, 5n, 120, 40))[0]).toBe(TERMINAL_TAG_RESIZE_REQUEST);
  });

});
