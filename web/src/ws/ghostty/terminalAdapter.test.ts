import { afterEach, describe, expect, it, vi } from "vitest";
import type { GhosttyTheme } from "./core";
import { GhosttyTerminalSurface } from "./surface";
import { GhosttyTerminalAdapter } from "./terminalAdapter";

vi.mock("./surface", () => ({ GhosttyTerminalSurface: { create: vi.fn() } }));

const theme: GhosttyTheme = {
  foreground: { r: 210, g: 210, b: 210 },
  background: { r: 10, g: 10, b: 10 },
  cursor: { r: 250, g: 180, b: 20 },
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function surfaceDouble() {
  const events: string[] = [];
  const surface = {
    cols: 80,
    rows: 24,
    fit: () => false,
    measuredGrid: () => ({ cols: 80, rows: 24 }),
    setTheme: (value: GhosttyTheme) => events.push(`theme:${value.background.r}`),
    setVisible: (visible: boolean) => events.push(`visible:${visible}`),
    focus: () => events.push("focus"),
    resize: (cols: number, rows: number) => events.push(`resize:${cols}x${rows}`),
    write: (data: string | Uint8Array) => events.push(`write:${typeof data === "string" ? data : data.length}`),
    reset: () => events.push("reset"),
    restoreSnapshot: (_data: Uint8Array, callback?: () => void) => callback?.(),
    resetAndWrite: (data: string | Uint8Array) => events.push(`reset-write:${data.length}`),
    dispose: () => events.push("dispose"),
    getSelection: () => "",
    hasSelection: () => false,
    clearSelection: () => {},
    scrollToBottom: () => {},
    scrollToLine: () => {},
    refresh: () => {},
    synchronizedOutputMode: () => false,
    mouseTrackingMode: () => "none" as const,
    isAlternateScreen: () => false,
    viewportState: () => ({ type: "normal", baseY: 0, viewportY: 0, length: 24 }),
    onRender: () => ({ dispose() {} }),
  };
  return { surface: surface as unknown as GhosttyTerminalSurface, events };
}

function mount(): HTMLDivElement {
  return {} as HTMLDivElement;
}

afterEach(() => vi.clearAllMocks());

describe("GhosttyTerminalAdapter", () => {
  it("applies current theme and visibility, then drains queued geometry before output", async () => {
    const pending = deferred<GhosttyTerminalSurface>();
    vi.mocked(GhosttyTerminalSurface.create).mockReturnValueOnce(pending.promise);
    const ready = vi.fn();
    const adapter = new GhosttyTerminalAdapter(mount(), theme, () => {}, () => {}, ready, () => {}, () => true);
    adapter.setTheme({ ...theme, background: { r: 20, g: 20, b: 20 } });
    adapter.setVisible(false);
    adapter.focus();
    adapter.resize(100, 30);
    const data = new Uint8Array([65]);
    adapter.write(data);
    const stub = surfaceDouble();
    pending.resolve(stub.surface);
    await pending.promise;
    await Promise.resolve();

    expect(stub.events.slice(0, 4)).toEqual([
      "theme:20", "visible:false", "resize:100x30", "write:1",
    ]);
    adapter.setVisible(true);
    expect(stub.events.at(-1)).toBe("visible:true");
    adapter.focus();
    expect(stub.events.at(-1)).toBe("focus");
    expect(ready).toHaveBeenCalledOnce();
    adapter.dispose();
  });

  it("does not reclaim focus when visibility is published again", async () => {
    const stub = surfaceDouble();
    vi.mocked(GhosttyTerminalSurface.create).mockResolvedValueOnce(stub.surface);
    const adapter = new GhosttyTerminalAdapter(mount(), theme, () => {}, () => {}, () => {}, () => {}, () => true);
    await Promise.resolve();
    adapter.focus();
    const focused = stub.events.filter((event) => event === "focus").length;

    adapter.setVisible(true);
    adapter.setVisible(false);
    adapter.setVisible(true);

    expect(stub.events.filter((event) => event === "focus")).toHaveLength(focused);
    adapter.dispose();
  });

  it.each([false, true])("honors a deferred focus request only while focus is unchanged (moved: %s)", async (moved) => {
    const pending = deferred<GhosttyTerminalSurface>();
    vi.mocked(GhosttyTerminalSurface.create).mockReturnValueOnce(pending.promise);
    const original = {} as Element;
    const ownerDocument = { activeElement: original };
    const host = { ownerDocument } as HTMLDivElement;
    const adapter = new GhosttyTerminalAdapter(host, theme, () => {}, () => {}, () => {}, () => {}, () => true);
    adapter.focus();
    if (moved) ownerDocument.activeElement = {} as Element;
    const stub = surfaceDouble();

    pending.resolve(stub.surface);
    await pending.promise;
    await Promise.resolve();

    expect(stub.events.filter((event) => event === "focus")).toHaveLength(moved ? 0 : 1);
    adapter.dispose();
  });

  it("reports a rejected snapshot and stops applying subsequent output", async () => {
    const pending = deferred<GhosttyTerminalSurface>();
    vi.mocked(GhosttyTerminalSurface.create).mockReturnValueOnce(pending.promise);
    const issue = vi.fn();
    const adapter = new GhosttyTerminalAdapter(mount(), theme, () => {}, issue, () => {}, () => {}, () => true);
    const stub = surfaceDouble();
    stub.surface.restoreSnapshot = () => { throw new Error("Invalid terminal snapshot"); };
    pending.resolve(stub.surface);
    await pending.promise;
    await Promise.resolve();
    expect(() => adapter.restoreSnapshot(new Uint8Array([0]))).toThrow("Invalid terminal snapshot");
    expect(issue).toHaveBeenCalledWith(expect.objectContaining({ message: "Invalid terminal snapshot" }));
    expect(stub.events).toContain("dispose");
    adapter.write("must not reach the old parser");
    expect(stub.events.some((event) => event.startsWith("write:"))).toBe(false);
  });

  it("keeps replayed command state while suppressing stale clipboard writes", async () => {
    const pending = deferred<GhosttyTerminalSurface>();
    vi.mocked(GhosttyTerminalSurface.create).mockReturnValueOnce(pending.promise);
    const adapter = new GhosttyTerminalAdapter(mount(), theme, () => {}, () => {}, () => {}, () => {}, () => true);
    const command = vi.fn(() => true);
    const clipboard = vi.fn(() => true);
    adapter.parser.registerOscHandler(777, command);
    adapter.parser.registerOscHandler(52, clipboard);
    const stub = surfaceDouble();
    pending.resolve(stub.surface);
    await pending.promise;
    await Promise.resolve();

    adapter.setReplaying(true);
    adapter.write("\u001b]777;pm-command;editor\u0007\u001b]52;c;c3RhbGU=\u0007");
    expect(command).toHaveBeenCalledWith("pm-command;editor");
    expect(clipboard).not.toHaveBeenCalled();
    adapter.setReplaying(false);
    adapter.write("\u001b]52;c;bmV3\u0007");
    expect(clipboard).toHaveBeenCalledWith("c;bmV3");
    adapter.dispose();
  });

  it("keeps replay OSC52 suppressed across multiple queued snapshots", async () => {
    const pending = deferred<GhosttyTerminalSurface>();
    vi.mocked(GhosttyTerminalSurface.create).mockReturnValueOnce(pending.promise);
    const adapter = new GhosttyTerminalAdapter(mount(), theme, () => {}, () => {}, () => {}, () => {}, () => true);
    const clipboard = vi.fn(() => true);
    adapter.parser.registerOscHandler(52, clipboard);

    for (const stale of ["first", "second"]) {
      adapter.setReplaying(true);
      adapter.write(`\u001b]52;c;${btoa(stale)}\u0007`);
      adapter.setReplaying(false);
    }
    adapter.write(`\u001b]52;c;${btoa("live")}\u0007`);

    const stub = surfaceDouble();
    pending.resolve(stub.surface);
    await pending.promise;
    await Promise.resolve();

    expect(clipboard).toHaveBeenCalledTimes(1);
    expect(clipboard).toHaveBeenCalledWith(`c;${btoa("live")}`);
    expect(stub.events.filter((event) => event.startsWith("write:")).length).toBe(3);
    adapter.dispose();
  });

  it("does not parse or publish output after disposal while WASM is loading", async () => {
    const pending = deferred<GhosttyTerminalSurface>();
    vi.mocked(GhosttyTerminalSurface.create).mockReturnValueOnce(pending.promise);
    const adapter = new GhosttyTerminalAdapter(mount(), theme, () => {}, () => {}, () => {}, () => {}, () => true);
    const command = vi.fn(() => true);
    adapter.parser.registerOscHandler(777, command);
    adapter.dispose();
    const stub = surfaceDouble();
    pending.resolve(stub.surface);
    await pending.promise;
    await Promise.resolve();
    expect(command).not.toHaveBeenCalled();
    expect(stub.events).toEqual(["dispose"]);
  });

  it("ignores a late resize notification after disposal", async () => {
    const onResize = vi.fn();
    const stub = surfaceDouble();
    vi.mocked(GhosttyTerminalSurface.create).mockResolvedValueOnce(stub.surface);
    const adapter = new GhosttyTerminalAdapter(mount(), theme, onResize, () => {}, () => {}, () => {}, () => true);
    const options = vi.mocked(GhosttyTerminalSurface.create).mock.calls.at(-1)?.[1];

    adapter.dispose();
    await Promise.resolve();
    options?.onResize(139, 47);

    expect(onResize).not.toHaveBeenCalled();
    expect(adapter.cols).toBe(80);
    expect(stub.events).toEqual(["dispose"]);
  });
});
