import { describe, expect, it, vi } from "vitest";
import type { PtyFrame } from "@puppet-master/client-core/ws/pty";
import { createTerminalProbeClient } from "../../bench/terminalProbeClient";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

describe("terminal benchmark fixture", () => {
  it("opens with a sized initial snapshot and echoes resize requests", async () => {
    const fixture = createTerminalProbeClient(() => encoder.encode("initial output"));
    const handle = fixture.client.openTerminal(1n, { cols: 120, rows: 40 });
    const frames: PtyFrame[] = [];
    handle.connect((frame) => frames.push(frame));
    await Promise.resolve();

    expect(handle.ptySize()).toEqual({ cols: 120, rows: 40 });
    expect(frames[0]).toMatchObject({ replay: true, snapshot: true });
    expect(decoder.decode(frames[0]!.data)).toContain("initial output");
    expect(handle.stats().replayCount).toBe(1);

    const echo = vi.fn();
    handle.onPtySize(echo);
    handle.resize(100, 30);
    await Promise.resolve();
    expect(echo).toHaveBeenCalledExactlyOnceWith({ cols: 100, rows: 30 });
    expect(handle.ptySize()).toEqual({ cols: 100, rows: 30 });
  });

  it("retains live output for requested snapshots and tracks terminal lifetimes", async () => {
    const fixture = createTerminalProbeClient();
    const handle = fixture.client.openPty(1n, { cols: 120, rows: 40 });
    const frames: PtyFrame[] = [];
    handle.connect((frame) => frames.push(frame));
    await Promise.resolve();
    fixture.write("s:1", encoder.encode("first line\r\n"), true);
    await Promise.resolve();
    fixture.write("s:1", encoder.encode("second line\r\n"));
    expect(frames.at(-1)?.replay).toBe(false);

    handle.refreshSnapshot();
    await Promise.resolve();
    expect(frames.at(-1)).toMatchObject({ replay: true, snapshot: true });
    expect(decoder.decode(frames.at(-1)!.data)).toContain("first line\r\nsecond line\r\n");
    handle.input(encoder.encode("input"));
    expect(fixture.inputs.get("s:1")).toBe(5);
    handle.close();
    expect(fixture.closes.get("s:1")).toBe(1);

    fixture.client.openTerminal(1n, { cols: 90, rows: 20 });
    expect(fixture.opens.get("s:1")).toBe(1);
    expect(fixture.opens.get("t:1")).toBe(1);
  });

  it("does not deliver a queued snapshot to a closed viewer", async () => {
    const fixture = createTerminalProbeClient();
    const handle = fixture.client.openTerminal(1n, { cols: 120, rows: 40 });
    const receive = vi.fn();
    handle.connect(receive);
    handle.close();
    await Promise.resolve();
    expect(receive).not.toHaveBeenCalled();
  });
});
