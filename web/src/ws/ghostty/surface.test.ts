// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import type { GhosttyCell, GhosttyRow, GhosttySnapshot, GhosttyTheme } from "./core";
import {
  GhosttyTerminalSurface,
  advanceTerminalSelectionClickSequence,
  handleTerminalHistoryKey,
  isModifiedTerminalLinkClick,
  resetTerminalSelectionClickSequenceAfterDrag,
  shouldActivateTerminalLink,
  shouldActivateTerminalLinkClick,
  terminalLinkAtPositionWithRange,
} from "./surface";

const theme: GhosttyTheme = {
  foreground: { r: 210, g: 210, b: 210 },
  background: { r: 10, g: 10, b: 10 },
  cursor: { r: 250, g: 180, b: 20 },
};

afterEach(() => vi.restoreAllMocks());

describe("GhosttyTerminalSurface lifecycle", () => {
  it("looks up a hyperlink on its zero-based viewport row", () => {
    const row = (text: string): GhosttyRow => ({
      text,
      cells: [...text].map((character): GhosttyCell => ({
        text: character,
        wide: 0,
        foreground: theme.foreground,
        background: theme.background,
        bold: false,
        italic: false,
        invisible: false,
        strikethrough: false,
        overline: false,
        underline: false,
        selected: false,
      })),
      isWrapContinuation: false,
      wrapsToNext: false,
    });
    const rows = [row("first row"), row("pm:item/1/2"), row("third row")];

    expect(terminalLinkAtPositionWithRange(rows, 1, 5)).toMatchObject({
      text: "pm:item/1/2",
      range: { start: { y: 1 }, end: { y: 1 } },
    });
  });

  it("publishes the new viewport when callers scroll history or return to the bottom", () => {
    const bottom = 100;
    let offset = bottom;
    const positions: number[] = [];
    const core = {
      scrollbarState: () => ({ total: bottom + 4, len: 4, offset }),
      isAlternateScreen: () => false,
      scroll: (delta: number) => { offset += delta; },
      scrollToBottom: () => { offset = bottom; },
    };
    const surface = Object.assign(Object.create(GhosttyTerminalSurface.prototype), {
      core,
      rows: 4,
      options: { onViewportChange: () => positions.push(surface.viewportState().viewportY) },
      requestRender: vi.fn(),
    }) as GhosttyTerminalSurface;

    surface.scrollToLine(12);
    surface.scrollToBottom();

    expect(positions).toEqual([12, bottom]);
  });

  it("measures local geometry without reflowing until the PTY size is echoed", () => {
    const core = { resize: vi.fn() };
    const surface = Object.assign(Object.create(GhosttyTerminalSurface.prototype), {
      disposed: false,
      visible: true,
      terminal: { style: { setProperty: vi.fn() } },
      mount: { clientWidth: 1_008, clientHeight: 608 },
      canvas: { width: 1_008, height: 608 },
      canvasConfigured: true,
      canvasDevicePixelRatio: 1,
      pendingCanvasConfiguration: null,
      forceFullRender: false,
      scrollbarDirty: false,
      measuredCols: 80,
      measuredRows: 24,
      metrics: { width: 10, height: 20, baseline: 15 },
      cols: 80,
      rows: 24,
      mountHeight: 604,
      resizeNotified: true,
      resizeNotifyTimer: null,
      core,
      notifyResize: vi.fn(),
      renderFrame: vi.fn(),
      requestRender: vi.fn(),
    }) as GhosttyTerminalSurface;

    surface.fit();

    expect(surface.measuredGrid()).toEqual({ cols: 100, rows: 30 });
    expect(surface.cols).toBe(80);
    expect(surface.rows).toBe(24);
    expect(core.resize).not.toHaveBeenCalled();

    surface.resize(120, 32);
    expect(surface.cols).toBe(120);
    expect(surface.rows).toBe(32);
    expect(core.resize).toHaveBeenCalledWith(120, 32, 10, 20);
  });

  it("uses the core's authoritative dimensions for cell coordinates", () => {
    const style = { setProperty: vi.fn() };
    const surface = Object.assign(Object.create(GhosttyTerminalSurface.prototype), {
      cols: 1,
      rows: 1,
      terminal: { style },
    }) as GhosttyTerminalSurface;
    const snapshot = { cols: 139, rows: 47 } as GhosttySnapshot;

    (surface as unknown as { syncGridFromSnapshot(snapshot: GhosttySnapshot): void })
      .syncGridFromSnapshot(snapshot);

    expect(surface.cols).toBe(139);
    expect(surface.rows).toBe(47);
    expect(style.setProperty).toHaveBeenCalledWith("--ghostty-grid-cols", "139");
    expect(style.setProperty).toHaveBeenCalledWith("--ghostty-grid-rows", "47");
  });

  it("keeps the input and IME anchor at the cursor when the cursor is hidden", () => {
    const input = { style: { width: "", height: "", left: "", top: "" } };
    const surface = Object.assign(Object.create(GhosttyTerminalSurface.prototype), {
      input,
      snapshot: { cursorVisible: false, cursorX: 5, cursorY: 3 },
      metrics: { width: 10, height: 20, baseline: 15 },
      originY: 4,
      inputLeft: -1,
      inputTop: -1,
    }) as GhosttyTerminalSurface;

    (surface as unknown as { positionInput(): void }).positionInput();

    expect(input.style).toEqual({ width: "10px", height: "20px", left: "54px", top: "64px" });
  });

  it("uses Shift+PageUp and Shift+PageDown for normal-buffer history", () => {
    const scrollViewport = vi.fn();

    for (const [key, delta] of [["PageUp", -24], ["PageDown", 24]] as const) {
      const event = new KeyboardEvent("keydown", { key, shiftKey: true, cancelable: true });
      const handled = handleTerminalHistoryKey(
        event,
        false,
        () => ({ total: 100, offset: 50, len: 24 }),
        scrollViewport,
      );
      expect(handled).toBe(true);
      expect(event.defaultPrevented).toBe(true);
      expect(scrollViewport).toHaveBeenLastCalledWith(delta);
    }
  });

  it("leaves Shift+PageUp available to the application on the alternate screen", () => {
    const readScrollbarState = vi.fn(() => ({ total: 100, offset: 50, len: 24 }));
    const event = new KeyboardEvent("keydown", { key: "PageUp", shiftKey: true, cancelable: true });
    const handled = handleTerminalHistoryKey(event, true, readScrollbarState, vi.fn());

    expect(handled).toBe(false);
    expect(readScrollbarState).not.toHaveBeenCalled();
    expect(event.defaultPrevented).toBe(false);
  });

  it("treats the first click after a link drag as a single click", () => {
    const firstClick = advanceTerminalSelectionClickSequence(null, {
      clientX: 120,
      clientY: 64,
      timeStamp: 100,
    });
    const afterDrag = resetTerminalSelectionClickSequenceAfterDrag(firstClick, {
      clientX: 125,
      clientY: 64,
    });
    const nextClick = advanceTerminalSelectionClickSequence(afterDrag, {
      clientX: 120,
      clientY: 64,
      timeStamp: 200,
    });

    expect(afterDrag).toBeNull();
    expect(nextClick.count).toBe(1);
  });

  it("reports a bounded selection diagnostic from the rendered snapshot", () => {
    const selectedCell = { text: "X", selected: true };
    const surface = Object.assign(Object.create(GhosttyTerminalSurface.prototype), {
      snapshot: {
        cols: 2,
        rows: 1,
        cursorX: 1,
        cursorY: 0,
        cursorVisible: false,
        rowData: [{ cells: [selectedCell, { text: " ", selected: false }] }],
      },
      core: { selectionText: () => "X" },
      cols: 2,
      rows: 1,
      measuredCols: 2,
      measuredRows: 1,
      originY: 4,
      metrics: { width: 8, height: 18, baseline: 14 },
      inputLeft: 12,
      inputTop: 4,
      selectionAnchorScreen: { x: 0, y: 0 },
      selectionEndScreen: { x: 0, y: 0 },
      selectionEnd: { x: 0, y: 0 },
      selectionPointer: { x: 16, y: 13 },
      writeCount: 4,
      resetCount: 1,
      resizeCount: 2,
    }) as GhosttyTerminalSurface;

    expect((surface as unknown as { diagnosticSnapshot(): object }).diagnosticSnapshot()).toMatchObject({
      cols: 2,
      rows: 1,
      originY: 4,
      metrics: { width: 8, height: 18 },
      input: { left: 12, top: 4 },
      operations: { writes: 4, resets: 1, resizes: 2 },
      selectionText: "X",
      cursor: { x: 1, y: 0, visible: false },
      selectedRows: [{ row: 0, text: "X", columns: [0] }],
    });
  });

  it("requires Shift or Alt to activate links while mouse tracking is active", () => {
    expect(shouldActivateTerminalLink(true, { shiftKey: true, altKey: false, metaKey: false, ctrlKey: false })).toBe(true);
    expect(shouldActivateTerminalLink(true, { shiftKey: false, altKey: true, metaKey: false, ctrlKey: false })).toBe(true);
    expect(shouldActivateTerminalLink(true, { shiftKey: false, altKey: false, metaKey: true, ctrlKey: false })).toBe(true);
    expect(shouldActivateTerminalLink(true, { shiftKey: false, altKey: false, metaKey: false, ctrlKey: true })).toBe(true);
    expect(shouldActivateTerminalLink(true, { shiftKey: false, altKey: false, metaKey: false, ctrlKey: false })).toBe(false);
    expect(shouldActivateTerminalLink(false, { shiftKey: false, altKey: false, metaKey: false, ctrlKey: false })).toBe(true);
  });

  it("lets modified link clicks bypass a prior plain double-click sequence", () => {
    const plain = { altKey: false, shiftKey: false, metaKey: false, ctrlKey: false };
    expect(shouldActivateTerminalLinkClick(2, plain)).toBe(false);
    expect(shouldActivateTerminalLinkClick(3, plain)).toBe(false);
    for (const modifier of ["altKey", "shiftKey", "metaKey", "ctrlKey"] as const) {
      const event = { ...plain, [modifier]: true };
      expect(isModifiedTerminalLinkClick(event)).toBe(true);
      expect(shouldActivateTerminalLinkClick(2, event)).toBe(true);
      expect(shouldActivateTerminalLinkClick(3, event)).toBe(true);
    }
  });

  it("leaves the host unchanged when Canvas 2D initialization fails", async () => {
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
    const mount = document.createElement("div");
    const existing = document.createElement("span");
    mount.append(existing);

    await expect(GhosttyTerminalSurface.create(mount, {
      theme,
      onData: () => {},
      onResize: () => {},
      beforeKey: () => true,
      onLinkActivate: () => {},
    })).rejects.toThrow("Canvas 2D is unavailable");

    expect(mount.childNodes).toHaveLength(1);
    expect(mount.firstChild).toBe(existing);
  });

  it("cancels a pending PTY resize without flushing it during disposal", () => {
    const onResize = vi.fn();
    const stopObservingResize = vi.fn();
    const removeEvents = vi.fn();
    const cancelRender = vi.fn();
    const core = { dispose: vi.fn() };
    const terminal = { remove: vi.fn() };
    const clearTimeout = vi.spyOn(window, "clearTimeout");
    const surface = Object.assign(Object.create(GhosttyTerminalSurface.prototype), {
      disposed: false,
      mount: document.createElement("div"),
      options: { onResize },
      stopObservingResize,
      dprMedia: null,
      reducedMotionMedia: null,
      selectionScrollTimer: null,
      resizeNotifyTimer: 12,
      compositionSuppressionTimer: null,
      removeEvents,
      linkHint: null,
      terminal,
      core,
      renderListeners: new Set(),
      cancelRender,
    }) as GhosttyTerminalSurface;

    surface.dispose();

    expect(clearTimeout).toHaveBeenCalledWith(12);
    expect(onResize).not.toHaveBeenCalled();
    expect(core.dispose).toHaveBeenCalledOnce();
    expect(terminal.remove).toHaveBeenCalledOnce();
  });
});
