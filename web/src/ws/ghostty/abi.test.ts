import { describe, expect, it } from "vitest";
import {
  GHOSTTY_DEC_MODE,
  GHOSTTY_MOUSE_ENCODER_OPTION,
  GHOSTTY_RENDER_STATE_DATA,
  GHOSTTY_ROW_METADATA,
  GHOSTTY_TERMINAL_DATA,
  GHOSTTY_TERMINAL_OPTION,
} from "./abi";

describe("pinned libghostty-vt ABI mappings", () => {
  it("maps terminal options and data selectors to the pinned C ABI", () => {
    expect(GHOSTTY_TERMINAL_OPTION).toMatchObject({
      foreground: 11,
      background: 12,
      cursor: 13,
      palette: 14,
      selection: 21,
      defaultCursorBlink: 23,
    });
    expect(GHOSTTY_TERMINAL_DATA).toMatchObject({
      activeScreen: 6,
      cursorX: 3,
      cursorY: 4,
      scrollbar: 9,
      mouseTracking: 11,
      defaultPalette: 25,
      viewportActive: 32,
    });
  });

  it("maps mode, render data, row metadata, and mouse options without shifting their C tags", () => {
    expect(GHOSTTY_DEC_MODE).toMatchObject({
      applicationCursorKeys: 1,
      anyMouseTracking: 1003,
      bracketedPaste: 2004,
      synchronizedOutput: 2026,
    });
    expect(GHOSTTY_RENDER_STATE_DATA).toMatchObject({
      cols: 1,
      rows: 2,
      rowIterator: 4,
      cursorStyle: 10,
      cursorX: 15,
      cursorY: 16,
    });
    expect(GHOSTTY_ROW_METADATA).toEqual({ wrapsToNext: 1, isWrapContinuation: 2 });
    expect(GHOSTTY_MOUSE_ENCODER_OPTION).toEqual({ size: 2, anyButtonPressed: 3, trackLastCell: 4 });
  });
});
