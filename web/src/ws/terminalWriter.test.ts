import type { Terminal } from "@xterm/xterm";
import { describe, expect, it, vi } from "vitest";
import { REPLAY_WRITE_CHUNK_BYTES, writeTerminalFrame } from "./terminalWriter";

describe("terminal frame writes", () => {
  it("passes live output through as one write", () => {
    const data = new Uint8Array([1, 2]);
    const terminal = {
      buffer: { active: { baseY: 0, viewportY: 0, type: "normal" } },
      write: vi.fn(),
      scrollToBottom: vi.fn(),
    } as unknown as Terminal;
    writeTerminalFrame(terminal, { data, replay: false });
    expect(terminal.write).toHaveBeenCalledWith(data, expect.any(Function));
  });

  it("buffers rendering across bounded replay writes", () => {
    let parsed: (() => void) | undefined;
    const terminal = {
      buffer: { active: { baseY: 20, viewportY: 15 } },
      write: vi.fn((_data: Uint8Array, callback?: () => void) => { parsed = callback; }),
      scrollToLine: vi.fn(),
      scrollToBottom: vi.fn(),
    } as unknown as Terminal;
    const data = new Uint8Array(REPLAY_WRITE_CHUNK_BYTES + 1);
    data[0] = 3;
    data[data.length - 1] = 4;

    writeTerminalFrame(terminal, { data, replay: true });

    expect(terminal.write).toHaveBeenCalledTimes(2);
    const first = vi.mocked(terminal.write).mock.calls[0][0] as Uint8Array;
    const last = vi.mocked(terminal.write).mock.calls[1][0] as Uint8Array;
    expect([...first.subarray(0, 11)]).toEqual([
      0x1b, 0x63, 0x1b, 0x5b, 0x3f, 0x32, 0x30, 0x32, 0x36, 0x68, 3,
    ]);
    expect([...last]).toEqual([4, 0x1b, 0x5b, 0x3f, 0x32, 0x30, 0x32, 0x36, 0x6c]);
    expect(vi.mocked(terminal.write).mock.calls[0][1]).toBeUndefined();
    expect(vi.mocked(terminal.write).mock.calls[1][1]).toEqual(expect.any(Function));
    (terminal.buffer.active as { baseY: number }).baseY = 30;
    parsed?.();
    expect(terminal.scrollToLine).toHaveBeenCalledWith(25);
  });

  it("re-pins an at-bottom viewport after live output triggers user-scroll accounting", () => {
    let parsed: (() => void) | undefined;
    const terminal = {
      buffer: { active: { baseY: 20, viewportY: 20, type: "normal" } },
      write: vi.fn((_data: Uint8Array, callback?: () => void) => { parsed = callback; }),
      scrollToLine: vi.fn(),
      scrollToBottom: vi.fn(),
    } as unknown as Terminal;

    writeTerminalFrame(terminal, { data: new Uint8Array([1]), replay: false });
    (terminal.buffer.active as { baseY: number; viewportY: number }).baseY = 5_000;
    (terminal.buffer.active as { baseY: number; viewportY: number }).viewportY = 0;
    parsed?.();

    expect(terminal.scrollToBottom).toHaveBeenCalledOnce();
  });

  it("does not overwrite a real user scroll that occurs while a write parses", () => {
    let parsed: (() => void) | undefined;
    const terminal = {
      buffer: { active: { baseY: 20, viewportY: 20, type: "normal" } },
      write: vi.fn((_data: Uint8Array, callback?: () => void) => { parsed = callback; }),
      scrollToLine: vi.fn(),
      scrollToBottom: vi.fn(),
    } as unknown as Terminal;

    writeTerminalFrame(
      terminal,
      { data: new Uint8Array([1]), replay: false },
      { bufferType: "normal", linesFromBottom: 0 },
      undefined,
      () => false,
    );
    parsed?.();

    expect(terminal.scrollToBottom).not.toHaveBeenCalled();
    expect(terminal.scrollToLine).not.toHaveBeenCalled();
  });
});
