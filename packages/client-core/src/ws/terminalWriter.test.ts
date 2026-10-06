import { describe, expect, it, vi } from "vitest";
import { REPLAY_WRITE_CHUNK_BYTES, writeTerminalFrame } from "./terminalWriter";

interface RecordedWrite {
  data: Uint8Array;
  callback?: () => void;
}

function recordingTarget(): { writes: RecordedWrite[]; write(data: Uint8Array, callback?: () => void): void } {
  const writes: RecordedWrite[] = [];
  return {
    writes,
    write(data: Uint8Array, callback?: () => void) {
      writes.push({ data, callback });
    },
  };
}

describe("terminal frame writes", () => {
  it("passes live output through as one write and restores the viewport after parse", () => {
    const terminal = recordingTarget();
    const restore = vi.fn();
    const parsed = vi.fn();
    const data = new Uint8Array([1, 2]);

    writeTerminalFrame(terminal, { data, replay: false }, "bookmark", restore, parsed);

    expect(terminal.writes).toHaveLength(1);
    expect(terminal.writes[0].data).toBe(data);
    expect(restore).not.toHaveBeenCalled();
    terminal.writes[0].callback?.();
    expect(restore).toHaveBeenCalledWith("bookmark");
    expect(parsed).toHaveBeenCalledWith("bookmark");
  });

  it("wraps bounded replay chunks in reset and synchronized-output framing", () => {
    const terminal = recordingTarget();
    const restore = vi.fn();
    const data = new Uint8Array(REPLAY_WRITE_CHUNK_BYTES + 1);
    data[0] = 3;
    data[data.length - 1] = 4;

    writeTerminalFrame(terminal, { data, replay: true }, "bookmark", restore);

    expect(terminal.writes).toHaveLength(2);
    expect([...terminal.writes[0].data.subarray(0, 11)]).toEqual([
      0x1b, 0x63, 0x1b, 0x5b, 0x3f, 0x32, 0x30, 0x32, 0x36, 0x68, 3,
    ]);
    expect([...terminal.writes[1].data]).toEqual([4, 0x1b, 0x5b, 0x3f, 0x32, 0x30, 0x32, 0x36, 0x6c]);
    expect(terminal.writes[0].callback).toBeUndefined();
    expect(terminal.writes[1].callback).toEqual(expect.any(Function));
    terminal.writes[1].callback?.();
    expect(restore).toHaveBeenCalledTimes(1);
  });

  it("skips the restore when shouldRestore reports a user scroll", () => {
    const terminal = recordingTarget();
    const restore = vi.fn();
    const parsed = vi.fn();

    writeTerminalFrame(
      terminal,
      { data: new Uint8Array([1]), replay: false },
      "bookmark",
      restore,
      parsed,
      () => false,
    );
    terminal.writes[0].callback?.();

    expect(restore).not.toHaveBeenCalled();
    expect(parsed).toHaveBeenCalledWith("bookmark");
  });
});
