import { describe, expect, it } from "vitest";
import type { Terminal } from "@xterm/xterm";
import { collectXtermDebug, debugChips, type TerminalLayerDebug } from "./terminalDiagnostics";

function fakeTerm(core: object): Terminal {
  return { _core: core } as unknown as Terminal;
}

describe("collectXtermDebug", () => {
  it("reads mouse, mode, renderer, and scrollable state", () => {
    const debug = collectXtermDebug(fakeTerm({
      coreMouseService: { activeProtocol: "ANY", activeEncoding: "SGR" },
      coreService: {
        decPrivateModes: { bracketedPasteMode: true, applicationCursorKeys: false, sendFocus: true },
      },
      _charSizeService: { hasValidSize: true },
      _renderService: {
        dimensions: { css: { cell: { width: 7.5, height: 17 }, canvas: { width: 1122, height: 816 } } },
      },
      _viewport: {
        _scrollableElement: {
          getScrollDimensions: () => ({ height: 816, scrollHeight: 28781 }),
          getScrollPosition: () => ({ scrollTop: 25144 }),
        },
      },
    }));
    expect(debug).toEqual({
      mouseProtocol: "ANY",
      mouseEncoding: "SGR",
      bracketedPaste: true,
      applicationCursorKeys: false,
      sendFocus: true,
      cellWidth: 7.5,
      cellHeight: 17,
      canvasWidth: 1122,
      canvasHeight: 816,
      charSizeValid: true,
      scrollableHeight: 816,
      scrollableScrollHeight: 28781,
      scrollableScrollTop: 25144,
    });
  });

  it("degrades every field to null when internals are missing", () => {
    const debug = collectXtermDebug(fakeTerm({}));
    expect(Object.values(debug).every((value) => value === null)).toBe(true);
  });

  it("survives internals that throw", () => {
    const debug = collectXtermDebug(fakeTerm({
      _viewport: {
        _scrollableElement: {
          getScrollDimensions: () => {
            throw new Error("disposed");
          },
          getScrollPosition: () => ({ scrollTop: 1 }),
        },
      },
    }));
    expect(debug.scrollableHeight).toBeNull();
    expect(debug.scrollableScrollTop).toBeNull();
  });
});

describe("debugChips", () => {
  const layer: TerminalLayerDebug = {
    key: "s:7",
    visible: true,
    cols: 187,
    rows: 48,
    bufferType: "alternate",
    bufferLines: 48,
    baseY: 0,
    viewportY: 0,
    backgroundBytes: 2048,
    lastPtyResize: { cols: 187, rows: 48 },
    socket: {
      generation: "3",
      phase: "online",
      socketsOpened: 2,
      socketsClosed: 1,
      outputBytes: 320_820,
      lastOutputAt: 9_000,
      replayCount: 1,
      lastReplayBytes: 262_144,
      lastReplayAt: 5_000,
      inputBytesSent: 24,
      inputBytesPending: 5,
      inputBytesDropped: 12,
      resizesSent: [{ cols: 187, rows: 47, at: 5_100 }, { cols: 187, rows: 48, at: 5_101 }],
      ptySize: { cols: 187, rows: 48 },
      lastReplaySnapshot: false,
      lastError: "",
    },
    mouseProtocol: "NONE",
    mouseEncoding: "DEFAULT",
    bracketedPaste: false,
    applicationCursorKeys: null,
    sendFocus: true,
    cellWidth: 7.5,
    cellHeight: 17,
    canvasWidth: 1122,
    canvasHeight: 816,
    charSizeValid: false,
    scrollableHeight: 816,
    scrollableScrollHeight: 816,
    scrollableScrollTop: 0,
  };

  it("renders the states that identify the known failure signatures", () => {
    const chips = debugChips(layer, 15_000);
    const joined = chips.join(" ");
    expect(joined).toContain("buf=alternate");
    expect(joined).toContain("mouse=NONE/DEFAULT");
    expect(joined).toContain("cell=7.5x17 INVALID");
    expect(joined).toContain("sock=online g3 open:2 closed:1");
    expect(joined).toContain("replay=1x last:256.0k 10s ago");
    expect(joined).toContain("in=24b held:5b dropped:12b");
    expect(joined).toContain("resizes=187x47,187x48");
    expect(joined).toContain("ptyEcho=187x48");
    expect(joined).toContain("bg=2.0k");
    expect(joined).toContain("modes=paste:off appcur:? focus:on");
  });

  it("marks hidden layers and missing sockets", () => {
    const chips = debugChips({ ...layer, visible: false, socket: null }, 15_000);
    expect(chips[0]).toBe("s:7 (hidden)");
    expect(chips).toContain("sock=?");
  });
});
