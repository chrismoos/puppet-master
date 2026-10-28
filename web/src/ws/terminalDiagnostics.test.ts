import { describe, expect, it } from "vitest";
import { collectTerminalSurfaceDebug, debugChips, type TerminalLayerDebug } from "./terminalDiagnostics";

function fakeTerminal(canvas: { width: number; height: number } | null) {
  return {
    element: { querySelector: () => canvas },
    cellSize: { cols: 80, rows: 24 },
    cellMetrics: { width: 8.4, height: 18 },
    modes: { mouseTrackingMode: "none" },
  } as never;
}

describe("collectTerminalSurfaceDebug", () => {
  it("reports Canvas dimensions and leaves unsupported protocol detail absent", () => {
    expect(collectTerminalSurfaceDebug(fakeTerminal({ width: 1122, height: 816 }))).toEqual({
      mouseProtocol: "none",
      mouseEncoding: null,
      bracketedPaste: null,
      applicationCursorKeys: null,
      sendFocus: null,
      cellWidth: 8.4,
      cellHeight: 18,
      canvasWidth: 1122,
      canvasHeight: 816,
      charSizeValid: true,
      scrollableHeight: null,
      scrollableScrollHeight: null,
      scrollableScrollTop: null,
    });
  });

  it("degrades the Canvas and size fields before initialization", () => {
    const debug = collectTerminalSurfaceDebug(fakeTerminal(null));
    expect(debug.canvasWidth).toBeNull();
    expect(debug.canvasHeight).toBeNull();
    expect(debug.charSizeValid).toBe(true);
  });
});

describe("debugChips", () => {
  it("shows terminal geometry and connection phase", () => {
    const layer: TerminalLayerDebug = {
      key: "s:4",
      visible: true,
      cols: 80,
      rows: 24,
      bufferType: "normal",
      bufferLines: 40,
      baseY: 16,
      viewportY: 16,
      backgroundBytes: 0,
      lastPtyResize: { cols: 80, rows: 24 },
      socket: null,
      screenText: ["line"],
      mouseProtocol: null,
      mouseEncoding: null,
      bracketedPaste: null,
      applicationCursorKeys: null,
      sendFocus: null,
      cellWidth: null,
      cellHeight: null,
      canvasWidth: 800,
      canvasHeight: 480,
      charSizeValid: true,
      scrollableHeight: null,
      scrollableScrollHeight: null,
      scrollableScrollTop: null,
    };
    expect(debugChips(layer, 100)).toContain("canvas=800x480");
    expect(debugChips(layer, 100)).toContain("pty=80x24");
  });
});
