import { readFile } from "node:fs/promises";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { GhosttyTerminalCore, MAX_SCROLLBACK_BYTES } from "./core";
import { GHOSTTY_DEC_MODE, GHOSTTY_GRID_REF_TAG } from "./abi";
import { loadGhosttyRuntime } from "./runtime";
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

  it("loads history incrementally while preserving newer live output", async () => {
    const runtime = await loadGhosttyRuntime();
    const slot = runtime.allocOpaque();
    runtime.call("ghostty_terminal_new", 0, slot, 80, 8);
    const terminal = runtime.readPointer(slot);
    const text = new TextEncoder().encode(Array.from({ length: 1500 }, (_, row) => `history ${String(row).padStart(4, "0")}\r\n`).join(""));
    const input = runtime.alloc(text.length);
    runtime.bytes(input, text.length).set(text);
    runtime.call("ghostty_terminal_vt_write", terminal, input, text.length);
    runtime.call("ghostty_snapshot_encode_buf", terminal, 0, 0, slot);
    const length = runtime.readPointer(slot);
    const encoded = runtime.alloc(length);
    expect(runtime.call("ghostty_snapshot_encode_buf", terminal, encoded, length, slot)).toBe(0);
    const bytes = runtime.bytes(encoded, length).slice();
    runtime.free(encoded, length);
    runtime.free(input, text.length);
    runtime.call("ghostty_terminal_free", terminal);
    runtime.freeOpaque(slot);
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const HEADER_BYTES = 10;
    const READY = 5;
    const PAGE = 3;
    const FINISH = 6;
    const IMAGE_PAYLOAD = 9;
    let ready = 0;
    let start = 0;
    const pages: Uint8Array[] = [];
    for (let offset = HEADER_BYTES; offset < bytes.length;) {
      const tag = view.getUint16(offset, true);
      const end = offset + HEADER_BYTES + view.getUint32(offset + 2, true);
      if (tag === READY) { ready = end; start = end; }
      else if (ready && [PAGE, FINISH, IMAGE_PAYLOAD].includes(tag)) { const boundary = tag === PAGE ? end + HEADER_BYTES : end; pages.push(bytes.slice(start, boundary)); start = boundary; }
      offset = end;
    }
    expect(ready).toBeGreaterThan(0);
    expect(pages.length).toBeGreaterThan(1);
    const core = await createCore();
    core.restoreSnapshot(bytes.slice(0, ready), 0);
    core.write("\r\nLIVE AFTER SNAPSHOT");
    const before = core.snapshot().rowData.map((row) => row.text).join("\n");
    expect(before).toContain("LIVE AFTER SNAPSHOT");
    for (const page of pages) core.appendHistory(page, 1);
    expect(core.snapshot().rowData.map((row) => row.text).join("\n")).toContain("LIVE AFTER SNAPSHOT");
    const reference = await createCore();
    reference.restoreSnapshot(bytes);
    reference.scroll(-100_000);
    core.scroll(-100_000);
    const expected = reference.snapshot().rowData.map((row) => row.text);
    expect(expected[0]).toMatch(/^history /);
    expect(core.snapshot().rowData.map((row) => row.text)).toEqual(expected);
  });

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

  it("restores a worker's hyperlinked scrollback and keeps writing into it", async () => {
    const bytes = new Uint8Array(await readFile(new URL("../../../../crates/pm-ghostty/testdata/hyperlink-history.snapshot", import.meta.url)));
    const core = await createCore();
    core.restoreSnapshot(bytes);
    const fixtureRows = 8;
    const fixtureHistory = 193;
    expect(core.snapshot().rows).toBe(fixtureRows);
    expect(core.scrollbarState()!.total).toBe(fixtureHistory + fixtureRows);
    expect(core.snapshot().rowData[fixtureRows - 2]?.text).toContain("link 0199 x");
    core.write("\x1b]8;;file:///fixture/after\x1b\\after\x1b]8;;\x1b\\");
    expect(core.snapshot().rowData[fixtureRows - 2]?.text).toContain("link 0199 x");
    expect(core.snapshot().rowData[fixtureRows - 1]?.text).toContain("after");
  });

  it("keeps its own scrollback limit after restoring a worker's snapshot", async () => {
    const workerLines = 5000;
    const written = workerLines + 1000;
    const bytes = new Uint8Array(await readFile(new URL("../../../../crates/pm-ghostty/testdata/native-state.snapshot", import.meta.url)));
    const core = await createCore();
    core.restoreSnapshot(bytes);
    core.write("\x1b[?1049l");
    core.write(Array.from({ length: written }, (_, line) => `line ${line}\r\n`).join(""));
    expect(core.scrollbarState()!.total).toBeGreaterThan(written);
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

  it("copies every interior wrapped line between partial selection endpoints", async () => {
    const core = await createCore();
    core.resize(20, 20, 8, 16);
    const lines = ["PREFIX first line", "long segment ".repeat(8) + "FIRST-END", "short", "second long segment ".repeat(4) + "SECOND-END", "last tail"];
    core.write(lines.join("\r\n"));
    const lastRow = lines.slice(0, -1).reduce((rows, line) => rows + Math.ceil(line.length / 20), 0);
    core.setSelection({ x: 7, y: 0 }, { x: 3, y: lastRow });
    expect(core.selectionText()).toBe([lines[0].slice(7), ...lines.slice(1, -1), lines.at(-1)!.slice(0, 4)].join("\n"));
  });

  it("copies complete wrapped logical lines across multiple pages of history and a resize", async () => {
    const core = await createCore();
    const lines = Array.from({ length: 60 }, (_, index) =>
      `ROW-${index}: ${"long-line-segment ".repeat(1 + index % 4)}END-${index}`);
    core.write(lines.join("\r\n"));
    core.selectAll();
    expect(core.selectionText().trimEnd()).toBe(lines.join("\n"));
    core.resize(31, 6, 8, 16);
    core.selectAll();
    expect(core.selectionText().trimEnd()).toBe(lines.join("\n"));
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

  it("observes each completed synchronized update before the next one starts", async () => {
    const core = await createCore();
    const completed: string[] = [];
    const observe = () => completed.push(core.snapshot().rowData[0]!.text.trimEnd());
    core.write("\x1b[?2026hfirst\x1b[?2026l\x1b[?2026h\rsecond\x1b[?2026l\x1b[?2026h\rpartial", observe);
    expect(completed).toEqual(["first", "second"]);
    expect(core.isSynchronizedOutput()).toBe(true);
  });

  it("observes a synchronized update whose closing sequence spans writes", async () => {
    const core = await createCore();
    const completed: string[] = [];
    const observe = () => completed.push(core.snapshot().rowData[0]!.text.trimEnd());
    for (const part of ["\x1b[?2026hready\x1b[?20", "26l\x1b[?2026h\rpartial"]) core.write(part, observe);
    expect(completed).toEqual(["ready"]);
    expect(core.isSynchronizedOutput()).toBe(true);
  });

  it("ends synchronized output and keeps parsing progress reports afterward", async () => {
    const core = await createCore();
    core.write("\u001b[?2026h");
    expect(core.isSynchronizedOutput()).toBe(true);
    core.endSynchronizedOutput();
    expect(core.isSynchronizedOutput()).toBe(false);
    core.write("\u001b]9;4;1;50\u0007after");
    expect(core.snapshot().rowData[0]?.text).toContain("after");
  });

  it("encodes typed text, control keys, and the terminal's cursor key mode", async () => {
    const key = (init: { key: string; code: string; ctrlKey?: boolean }) => ({
      shiftKey: false,
      ctrlKey: false,
      altKey: false,
      metaKey: false,
      repeat: false,
      isComposing: false,
      getModifierState: () => false,
      ...init,
    }) as unknown as KeyboardEvent;
    const core = await createCore();
    expect(core.encodeKey(key({ key: "a", code: "KeyA" }))).toBe("a");
    expect(core.encodeKey(key({ key: "c", code: "KeyC", ctrlKey: true }))).toBe("\u0003");
    expect(core.encodeKey(key({ key: "ArrowUp", code: "ArrowUp" }))).toBe("\u001b[A");
    core.write("\u001b[?1h");
    expect(core.encodeKey(key({ key: "ArrowUp", code: "ArrowUp" }))).toBe("\u001bOA");
  });

  it("uses the bracketed paste mode when encoding pasted text", async () => {
    const core = await createCore();
    core.write("\u001b[?2004h");
    expect(core.encodePaste("draft")).toBe("\u001b[200~draft\u001b[201~");
  });

  it.each([7.8, 8.2].flatMap((width) => [0, 80, 150, 199].map((column) => ({ width, column }))))("reports column $column at character width $width", async ({ width, column }) => {
    const core = await createCore();
    core.resize(200, 4, 8, 16);
    core.write("\u001b[?1000h\u001b[?1006h");
    const padding = 4;
    const encoded = core.encodeMouse({
      action: "press", button: 1, mods: 0,
      x: padding + (column + 0.5) * width, y: padding + 1.5 * 16,
      screenWidth: padding * 2 + 200 * width, screenHeight: padding * 2 + 4 * 16,
      cellWidth: width, cellHeight: 16,
      paddingTop: padding, paddingBottom: padding, paddingRight: padding, paddingLeft: padding,
      anyButtonPressed: true,
    });
    expect(new TextDecoder().decode(encoded)).toBe(`\u001b[<0;${column + 1};2M`);
  });

  it("keeps pixel mouse coordinates independent of fractional cell widths", async () => {
    const core = await createCore();
    core.resize(200, 4, 8, 16);
    core.write("\u001b[?1000h\u001b[?1016h");
    const input = {
      action: "press" as const, button: 1, mods: 0,
      x: 1000, y: 24, screenWidth: 1600, screenHeight: 64,
      cellWidth: 8, cellHeight: 16,
      paddingTop: 4, paddingBottom: 4, paddingRight: 4, paddingLeft: 4,
      anyButtonPressed: true,
    };
    const integer = core.encodeMouse(input);
    const fractional = core.encodeMouse({ ...input, cellWidth: 7.8 });
    expect(fractional).toEqual(integer);
    expect(core.encodeMouse({ ...input, cellWidth: 7.8 })).toEqual(integer);
    core.write("\u001b[?1006h");
    expect(new TextDecoder().decode(core.encodeMouse({ ...input, cellWidth: 7.8 }))).toBe("\u001b[<0;128;2M");
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
