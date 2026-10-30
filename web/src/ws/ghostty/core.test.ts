import { readFile } from "node:fs/promises";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { GhosttyTerminalCore, MAX_SCROLLBACK_BYTES } from "./core";
import { GHOSTTY_DEC_MODE, GHOSTTY_GRID_REF_TAG } from "./abi";
import { GhosttyTerminalSurface } from "./surface";

vi.mock("./vendor/ghostty-vt.wasm?url", async () => ({
  default: "ghostty-core-wasm",
}));
vi.mock("./vendor/ghostty-write-pty.wasm?url&no-inline", async () => ({
  default: "ghostty-pty-wasm",
}));

const theme = {
  foreground: { r: 220, g: 220, b: 220 },
  background: { r: 10, g: 10, b: 10 },
  cursor: { r: 240, g: 180, b: 20 },
};

describe("Ghostty terminal core", () => {
  const cores = new Set<GhosttyTerminalCore>();
  let ghosttyWasm = new Uint8Array();
  let ptyWasm = new Uint8Array();

  beforeAll(async () => {
    ghosttyWasm = new Uint8Array(await readFile(new URL("./vendor/ghostty-vt.wasm", import.meta.url)));
    ptyWasm = new Uint8Array(await readFile(new URL("./vendor/ghostty-write-pty.wasm", import.meta.url)));
    vi.stubGlobal("fetch", vi.fn(async (url: string) => {
      const bytes = url === "ghostty-core-wasm" ? ghosttyWasm : ptyWasm;
      return new Response(bytes, { status: 200 });
    }));
  });

  async function createCore(
    onPtyData: (data: string) => void = () => {},
    maxScrollbackBytes = MAX_SCROLLBACK_BYTES,
  ) {
    const core = await GhosttyTerminalCore.create(20, 4, 8, 16, theme, onPtyData, { maxScrollbackBytes });
    cores.add(core);
    return core;
  }

  afterEach(() => {
    for (const core of cores) core.dispose();
    cores.clear();
  });

  afterAll(() => vi.unstubAllGlobals());

  it("restores native backend state and continues its unfinished parser", async () => {
    const bytes = new Uint8Array(await readFile(new URL("../../../../crates/pm-ghostty/testdata/native-state.snapshot", import.meta.url)));
    const core = await createCore();
    core.write("stale browser state");
    core.restoreSnapshot(bytes);
    expect(core.isAlternateScreen()).toBe(true);
    expect(core.mouseTrackingMode()).toBe("any");
    expect(core.snapshot().rowData[0]?.text).toContain("alternate state");
    expect(core.cursorPosition()).toEqual({ x: 4, y: 2 });
    core.write(";7mcontinued");
    const snapshot = core.snapshot();
    expect(snapshot.rowData[2]?.text).toContain("    continued");
    expect(snapshot.rowData[2]?.cells[4]).toMatchObject({
      foreground: { r: 9, g: 8, b: 7 }, background: { r: 10, g: 20, b: 30 },
      bold: true, italic: true, underline: true,
    });
    core.write("\x1b[?1049l");
    expect(core.snapshot().rowData[0]?.text).toContain("primary state");
  });

  it("rejects corrupt, truncated, and trailing snapshot data without replacing the current terminal", async () => {
    const bytes = new Uint8Array(await readFile(new URL("../../../../crates/pm-ghostty/testdata/native-state.snapshot", import.meta.url)));
    const corrupt = bytes.slice();
    corrupt[corrupt.length - 1] ^= 1;
    const trailing = new Uint8Array(bytes.length + 1);
    trailing.set(bytes);
    const core = await createCore();
    core.write("keep this screen");
    for (const invalid of [corrupt, bytes.subarray(0, bytes.length - 1), trailing]) {
      expect(() => core.restoreSnapshot(invalid)).toThrow();
      expect(core.snapshot().rowData[0]?.text).toContain("keep this screen");
    }
    core.write("!");
    expect(core.snapshot().rowData[0]?.text).toContain("keep this screen!");
  });

  it("parses UTF-8 text and ANSI colors into a render snapshot", async () => {
    const core = await createCore();
    core.write(new TextEncoder().encode("Hello \u{1f642}\u001b[31mred"));
    const snapshot = core.snapshot();
    expect(snapshot.cols).toBe(20);
    expect(snapshot.rowData[0]?.text).toContain("Hello");
    expect(snapshot.rowData[0]?.text).toContain("🙂");
    expect(snapshot.rowData[0]?.cells[9]?.foreground).not.toEqual(snapshot.foreground);
  });

  it("keeps live dirty rows for rendering after a cached debug snapshot read", async () => {
    const core = await createCore();
    const surface = Object.assign(Object.create(GhosttyTerminalSurface.prototype), {
      core,
      snapshot: core.snapshot(),
    }) as GhosttyTerminalSurface;
    core.write("LIVE-MARKER");
    const snapshot = vi.spyOn(core, "snapshot");

    expect(surface.renderSnapshot().rowData[0]?.text).not.toContain("LIVE-MARKER");
    expect(snapshot).not.toHaveBeenCalled();

    const painted = core.snapshot();
    expect(painted.dirtyRows.size).toBeGreaterThan(0);
    expect(painted.rowData[0]?.text).toContain("LIVE-MARKER");
  });

  it.each([
    [GHOSTTY_DEC_MODE.x10MouseTracking, "x10"],
    [GHOSTTY_DEC_MODE.normalMouseTracking, "vt200"],
    [GHOSTTY_DEC_MODE.buttonMouseTracking, "drag"],
    [GHOSTTY_DEC_MODE.anyMouseTracking, "any"],
  ] as const)("reports mouse mode %s from the native terminal", async (mode, name) => {
    const core = await createCore();
    expect(core.mouseTrackingMode()).toBe("none");
    core.write(`\x1b[?${mode}h`);
    expect(core.mouseTrackingMode()).toBe(name);
    core.write(`\x1b[?${mode}l`);
    expect(core.mouseTrackingMode()).toBe("none");
  });

  it("reads the terminal input cursor when DEC hides it from the render snapshot", async () => {
    const core = await createCore();
    core.write("\u001b[?25lREADY\r\n\r\n\r\n");

    const snapshot = core.snapshot();
    expect(snapshot.cursorVisible).toBe(false);
    expect(snapshot.cursorX).toBe(0);
    expect(snapshot.cursorY).toBe(3);
    expect(core.cursorPosition()).toEqual({ x: 0, y: 3 });
  });

  it("selects cells using viewport points at their row coordinates", async () => {
    const core = await createCore();
    core.write("ROW-0\r\nROW-1\r\nROW-2\r\nROW-3");
    core.snapshot();

    core.setSelection({ x: 0, y: 2 }, { x: 5, y: 2 });

    const snapshot = core.snapshot();
    expect(snapshot.rowData[0]?.cells.some((cell) => cell.selected)).toBe(false);
    expect(snapshot.rowData[2]?.cells.some((cell) => cell.selected)).toBe(true);
    expect(snapshot.rowData[2]?.text).toContain("ROW-2");
  });

  it("selects cells using screen points at their row coordinates", async () => {
    const core = await createCore();
    core.write("ROW-0\r\nROW-1\r\nROW-2\r\nROW-3");
    core.snapshot();

    core.setSelection(
      { x: 0, y: 2, tag: GHOSTTY_GRID_REF_TAG.screen },
      { x: 5, y: 2, tag: GHOSTTY_GRID_REF_TAG.screen },
    );

    const snapshot = core.snapshot();
    expect(snapshot.rowData[0]?.cells.some((cell) => cell.selected)).toBe(false);
    expect(snapshot.rowData[2]?.cells.some((cell) => cell.selected)).toBe(true);
    expect(snapshot.rowData[2]?.text).toContain("ROW-2");
  });

  it("renders retained history and preserves its visible text during live output", async () => {
    const core = await createCore();
    core.write(Array.from({ length: 16 }, (_, index) => `ROW-${index}`).join("\r\n"));
    core.snapshot();
    const bottom = core.scrollbarState();
    expect(bottom).not.toBeNull();
    expect(bottom!.offset).toBeGreaterThan(0);

    core.scroll(-bottom!.offset);
    const reading = core.snapshot();
    expect(reading.rowData.map((row) => row.text)).toEqual(["ROW-0", "ROW-1", "ROW-2", "ROW-3"]);
    expect(core.scrollbarState()?.offset).toBe(0);

    core.write("\r\nROW-16\r\nROW-17");
    expect(core.snapshot().rowData.map((row) => row.text)).toEqual(reading.rowData.map((row) => row.text));
    expect(core.scrollbarState()?.offset).toBe(0);

    core.scrollToBottom();
    expect(core.snapshot().rowData.at(-1)?.text).toBe("ROW-17");
  });

  it("maps a viewport selection to the same rendered row after history grows", async () => {
    const core = await createCore();
    core.write(Array.from({ length: 16 }, (_, index) => `ROW-${index}`).join("\r\n"));
    const before = core.snapshot();
    const screenPoint = core.viewportPointToScreen(0, 1);
    expect(screenPoint).not.toBeNull();
    expect(screenPoint!.y).toBeGreaterThan(1);

    core.setSelection(
      { ...screenPoint!, tag: GHOSTTY_GRID_REF_TAG.screen },
      { ...screenPoint!, x: 5, tag: GHOSTTY_GRID_REF_TAG.screen },
    );

    const after = core.snapshot();
    expect(after.rowData[0]?.cells.some((cell) => cell.selected)).toBe(false);
    expect(after.rowData[1]?.cells.some((cell) => cell.selected)).toBe(true);
    expect(after.rowData[1]?.text).toEqual(before.rowData[1]?.text);
  });

  it("formats the marker selected from a nonzero viewport row after coordinate conversion", async () => {
    const core = await createCore();
    core.write("READY\r\nECHO MARKER\r\nOUT MARKER\r\nEND");
    core.snapshot();
    const screenPoint = core.viewportPointToScreen(4, 2);
    expect(screenPoint).toEqual({ x: 4, y: 2 });

    core.setSelection(
      { ...screenPoint!, tag: GHOSTTY_GRID_REF_TAG.screen },
      { x: 9, y: screenPoint!.y, tag: GHOSTTY_GRID_REF_TAG.screen },
    );

    expect(core.selectionText()).toBe("MARKER");
  });

  it("evicts old rows when the byte-bounded scrollback fills", async () => {
    const core = await createCore(() => {}, 4 * 1024);
    core.write("OLDEST-EVICTION-MARKER\r\n");
    core.write(Array.from({ length: 20_000 }, (_, index) => `ROW-${index.toString().padStart(5, "0")}\r\n`).join(""));
    core.scroll(-10_000);

    const oldestVisibleRows = core.snapshot().rowData.map((row) => row.text).join("\n");
    const scrollbar = core.scrollbarState();

    expect(oldestVisibleRows).not.toContain("OLDEST-EVICTION-MARKER");
    expect(oldestVisibleRows).toContain("ROW-");
    expect(scrollbar?.total).toBeLessThan(20_000);
    expect(scrollbar?.total).toBeGreaterThan(4);
  });

  it("routes device status replies to the PTY callback", async () => {
    const output: string[] = [];
    const core = await createCore((data) => output.push(data));
    core.write("\u001b[5n");
    expect(output).toEqual(["\u001b[0n"]);
  });

  it("uses the bracketed paste mode when encoding pasted text", async () => {
    const core = await createCore();
    core.write("\u001b[?2004h");
    expect(core.encodePaste("draft")).toBe("\u001b[200~draft\u001b[201~");
  });

  it("preserves raw legacy mouse bytes for X10 coordinates above 95", async () => {
    const core = await createCore();
    core.resize(200, 4, 8, 16);
    core.write("\u001b[?9h");
    const encoded = core.encodeMouse({
      action: "press",
      button: 1,
      mods: 0,
      x: 1000,
      y: 24,
      screenWidth: 1600,
      screenHeight: 64,
      cellWidth: 8,
      cellHeight: 16,
      paddingTop: 0,
      paddingBottom: 0,
      paddingRight: 0,
      paddingLeft: 0,
      anyButtonPressed: true,
    });

    expect(encoded).toBeInstanceOf(Uint8Array);
    expect([...encoded.slice(0, 3)]).toEqual([0x1b, 0x5b, 0x4d]);
    expect(encoded.some((byte) => byte > 0x7f)).toBe(true);
  });
});
