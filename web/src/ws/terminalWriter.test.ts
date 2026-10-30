import { describe, expect, it, vi } from "vitest";
import { SYNC_OUTPUT_START } from "@puppet-master/client-core/ws/terminalWriter";
import { REPLAY_WRITE_CHUNK_BYTES, writeTerminalFrame } from "./terminalWriter";

function terminal() {
  const operations: Array<{ kind: string; data?: Uint8Array; callback?: () => void }> = [];
  const active = { baseY: 20, viewportY: 8, type: "normal" };
  return {
    operations,
    buffer: { active },
    rows: 24,
    write: vi.fn((data: string | Uint8Array, callback?: () => void) => {
      operations.push({ kind: "write", data: typeof data === "string" ? new TextEncoder().encode(data) : data, callback });
      callback?.();
    }),
    restoreSnapshot: vi.fn((data: Uint8Array, callback?: () => void) => {
      operations.push({ kind: "snapshot", data });
      callback?.();
    }),
    reset: vi.fn(() => operations.push({ kind: "reset" })),
    setReplaying: vi.fn((replaying: boolean) => operations.push({ kind: `replay:${replaying}` })),
    scrollToBottom: vi.fn(),
    scrollToLine: vi.fn(),
  };
}

describe("terminal frame writes", () => {
  it("restores binary snapshots without feeding them to the VT parser", () => {
    const view = terminal();
    const data = new Uint8Array([0, 255, 1]);
    const parsed = vi.fn();
    writeTerminalFrame(view, { data, replay: true, snapshot: true, ghosttySnapshot: true }, undefined, parsed);
    expect(view.restoreSnapshot).toHaveBeenCalledWith(data, expect.any(Function));
    expect(view.write).not.toHaveBeenCalled();
    expect(view.reset).not.toHaveBeenCalled();
    expect(view.operations.map(({ kind }) => kind)).toEqual(["replay:true", "snapshot", "replay:false"]);
    expect(parsed).toHaveBeenCalledOnce();
  });

  it("parses live bytes before acknowledging and preserves a reader's viewport", () => {
    const view = terminal();
    const data = new Uint8Array([1, 2]);
    const parsed = vi.fn();
    writeTerminalFrame(view, { data, replay: false }, undefined, parsed);
    expect(view.operations.map((operation) => operation.kind)).toEqual(["write"]);
    expect(view.operations[0]?.data).toBe(data);
    expect(parsed).toHaveBeenCalledOnce();
    expect(view.scrollToLine).not.toHaveBeenCalled();
    expect(view.scrollToBottom).not.toHaveBeenCalled();
  });

  it("resets and bounds replay writes inside synchronized output", () => {
    const view = terminal();
    const data = new Uint8Array(REPLAY_WRITE_CHUNK_BYTES + 1);
    data[0] = 3;
    data[data.length - 1] = 4;
    const parsed = vi.fn();
    const prepareReplay = vi.fn(() => view.operations.push({ kind: "prepare" }));
    writeTerminalFrame(view, { data, replay: true }, undefined, parsed, undefined, prepareReplay);

    expect(view.operations.map((operation) => operation.kind)).toEqual([
      "reset", "replay:true", "write", "prepare", "write", "write", "write", "replay:false",
    ]);
    expect(view.operations[2]?.data).toEqual(SYNC_OUTPUT_START);
    expect([...view.operations[4]!.data!]).toEqual([3, ...new Array(REPLAY_WRITE_CHUNK_BYTES - 1).fill(0)]);
    expect([...view.operations[5]!.data!]).toEqual([4]);
    expect(view.operations[6]?.data).toEqual(new Uint8Array([0x1b, 0x5b, 0x3f, 0x32, 0x30, 0x32, 0x36, 0x6c]));
    expect(prepareReplay).toHaveBeenCalledOnce();
    expect(parsed).toHaveBeenCalledOnce();
  });

  it("restores a saved scrollback distance after replay", () => {
    const view = terminal();
    view.buffer.active.baseY = 30;
    writeTerminalFrame(view, { data: new Uint8Array(), replay: true }, {
      bufferType: "normal",
      linesFromBottom: 5,
    });
    expect(view.scrollToLine).toHaveBeenCalledWith(25);
  });

  it("leaves a newer user scroll in place when the write finishes", () => {
    const view = terminal();
    const originalWrite = view.write;
    let parsed: (() => void) | undefined;
    view.write = vi.fn((data, callback) => {
      parsed = callback;
      originalWrite(data);
    });
    writeTerminalFrame(view, { data: new Uint8Array([1]), replay: false }, undefined, undefined, () => false);
    parsed?.();
    expect(view.scrollToBottom).not.toHaveBeenCalled();
    expect(view.scrollToLine).not.toHaveBeenCalled();
  });
});
