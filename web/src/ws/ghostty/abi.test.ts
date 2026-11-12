import { readFile } from "node:fs/promises";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import {
  GHOSTTY_CURSOR_STYLE,
  GHOSTTY_DEC_MODE,
  GHOSTTY_GRID_REF_TAG,
  GHOSTTY_KEY_EVENT_ACTION,
  GHOSTTY_MOUSE_ENCODER_OPTION,
  GHOSTTY_MOUSE_EVENT_ACTION,
  GHOSTTY_MOUSE_EVENT_BUTTON,
  GHOSTTY_RENDER_STATE_DATA,
  GHOSTTY_RENDER_STATE_DIRTY,
  GHOSTTY_RENDER_STATE_ROW_GET,
  GHOSTTY_RENDER_STATE_ROW_SET,
  GHOSTTY_RENDER_STATE_SET,
  GHOSTTY_ROW_METADATA,
  GHOSTTY_SCROLL_VIEWPORT_TAG,
  GHOSTTY_SNAPSHOT_DECODER_DATA,
  GHOSTTY_TERMINAL_DATA,
  GHOSTTY_TERMINAL_OPTION,
  GHOSTTY_TERMINAL_SCREEN,
} from "./abi";
import { GhosttyRuntime } from "./runtime";

vi.mock("./vendor/ghostty-vt.wasm?url", () => ({ default: "ghostty-core-wasm" }));
vi.mock("./vendor/ghostty-write-pty.wasm?url&no-inline", () => ({ default: "ghostty-pty-wasm" }));

type Mapping = readonly [table: Readonly<Record<string, number>>, enumType: string, names: Readonly<Record<string, string>>];

const MAPPINGS: Readonly<Record<string, Mapping>> = {
  GHOSTTY_TERMINAL_OPTION: [GHOSTTY_TERMINAL_OPTION, "GhosttyTerminalOption", {
    foreground: "COLOR_FOREGROUND",
    background: "COLOR_BACKGROUND",
    cursor: "COLOR_CURSOR",
    palette: "COLOR_PALETTE",
    selection: "SELECTION",
    defaultCursorBlink: "DEFAULT_CURSOR_BLINK",
    scrollbackMaxBytes: "SCROLLBACK_MAX_BYTES",
    scrollbackMaxLines: "SCROLLBACK_MAX_LINES",
    mode: "MODE",
  }],
  GHOSTTY_TERMINAL_DATA: [GHOSTTY_TERMINAL_DATA, "GhosttyTerminalData", {
    activeScreen: "ACTIVE_SCREEN",
    scrollbar: "SCROLLBAR",
    mouseTracking: "MOUSE_TRACKING",
    cursorX: "CURSOR_X",
    cursorY: "CURSOR_Y",
    viewportActive: "VIEWPORT_ACTIVE",
    mode: "MODE",
    defaultPalette: "COLOR_PALETTE_DEFAULT",
  }],
  GHOSTTY_SCROLL_VIEWPORT_TAG: [GHOSTTY_SCROLL_VIEWPORT_TAG, "GhosttyTerminalScrollViewportTag", {
    top: "TOP",
    bottom: "BOTTOM",
    delta: "DELTA",
  }],
  GHOSTTY_RENDER_STATE_SET: [GHOSTTY_RENDER_STATE_SET, "GhosttyRenderStateOption", { clearDirty: "DIRTY" }],
  GHOSTTY_RENDER_STATE_ROW_SET: [GHOSTTY_RENDER_STATE_ROW_SET, "GhosttyRenderStateRowOption", { clearDirty: "DIRTY" }],
  GHOSTTY_RENDER_STATE_DIRTY: [GHOSTTY_RENDER_STATE_DIRTY, "GhosttyRenderStateDirty", { rows: "FULL" }],
  GHOSTTY_RENDER_STATE_ROW_GET: [GHOSTTY_RENDER_STATE_ROW_GET, "GhosttyRenderStateRowData", {
    dirty: "DIRTY",
    raw: "RAW",
    cells: "CELLS",
  }],
  GHOSTTY_RENDER_STATE_DATA: [GHOSTTY_RENDER_STATE_DATA, "GhosttyRenderStateData", {
    cols: "COLS",
    rows: "ROWS",
    dirty: "DIRTY",
    rowIterator: "ROW_ITERATOR",
    background: "COLOR_BACKGROUND",
    foreground: "COLOR_FOREGROUND",
    cursor: "COLOR_CURSOR",
    cursorHasValue: "COLOR_CURSOR_HAS_VALUE",
    cursorStyle: "CURSOR_VISUAL_STYLE",
    cursorVisible: "CURSOR_VISIBLE",
    cursorBlinking: "CURSOR_BLINKING",
    cursorInViewport: "CURSOR_VIEWPORT_HAS_VALUE",
    cursorX: "CURSOR_VIEWPORT_X",
    cursorY: "CURSOR_VIEWPORT_Y",
  }],
  GHOSTTY_ROW_METADATA: [GHOSTTY_ROW_METADATA, "GhosttyRowData", {
    wrapsToNext: "WRAP",
    isWrapContinuation: "WRAP_CONTINUATION",
  }],
  GHOSTTY_MOUSE_EVENT_ACTION: [GHOSTTY_MOUSE_EVENT_ACTION, "GhosttyMouseAction", {
    press: "PRESS",
    release: "RELEASE",
    motion: "MOTION",
  }],
  GHOSTTY_KEY_EVENT_ACTION: [GHOSTTY_KEY_EVENT_ACTION, "GhosttyKeyAction", {
    release: "RELEASE",
    press: "PRESS",
    repeat: "REPEAT",
  }],
  GHOSTTY_MOUSE_EVENT_BUTTON: [GHOSTTY_MOUSE_EVENT_BUTTON, "GhosttyMouseButton", {
    left: "LEFT",
    right: "RIGHT",
    middle: "MIDDLE",
  }],
  GHOSTTY_TERMINAL_SCREEN: [GHOSTTY_TERMINAL_SCREEN, "GhosttyTerminalScreen", {
    primary: "PRIMARY",
    alternate: "ALTERNATE",
  }],
  GHOSTTY_CURSOR_STYLE: [GHOSTTY_CURSOR_STYLE, "GhosttyRenderStateCursorVisualStyle", {
    bar: "BAR",
    block: "BLOCK",
    underline: "UNDERLINE",
    hollowBlock: "BLOCK_HOLLOW",
  }],
  GHOSTTY_GRID_REF_TAG: [GHOSTTY_GRID_REF_TAG, "GhosttyPointTag", {
    viewport: "VIEWPORT",
    screen: "SCREEN",
  }],
  GHOSTTY_MOUSE_ENCODER_OPTION: [GHOSTTY_MOUSE_ENCODER_OPTION, "GhosttyMouseEncoderOption", {
    size: "SIZE",
    anyButtonPressed: "ANY_BUTTON_PRESSED",
    trackLastCell: "TRACK_LAST_CELL",
  }],
  GHOSTTY_SNAPSHOT_DECODER_DATA: [GHOSTTY_SNAPSHOT_DECODER_DATA, "GhosttySnapshotDecoderData", {
    sourceOffset: "SOURCE_OFFSET",
  }],
};

describe("libghostty-vt ABI mappings", () => {
  let runtime: GhosttyRuntime;

  beforeAll(async () => {
    const coreWasm = new Uint8Array(await readFile(new URL("./vendor/ghostty-vt.wasm", import.meta.url)));
    const ptyWasm = new Uint8Array(await readFile(new URL("./vendor/ghostty-write-pty.wasm", import.meta.url)));
    vi.stubGlobal("fetch", vi.fn(async (url: string) => new Response(
      url === "ghostty-core-wasm" ? coreWasm : ptyWasm,
      { status: 200 },
    )));
    runtime = await GhosttyRuntime.load();
  });

  afterAll(() => vi.unstubAllGlobals());

  it.each(Object.entries(MAPPINGS))("matches the vendored wasm's %s enum values by name", (_, [table, enumType, names]) => {
    expect(Object.keys(names).sort()).toEqual(Object.keys(table).sort());
    const values = runtime.layout(enumType).values ?? {};
    const expected = Object.fromEntries(Object.entries(names).map(([key, name]) => [key, values[name]]));
    expect(table).toEqual(expected);
  });

  it("uses the DEC private mode numbers terminals send", () => {
    expect(GHOSTTY_DEC_MODE).toEqual({
      applicationCursorKeys: 1,
      x10MouseTracking: 9,
      normalMouseTracking: 1000,
      buttonMouseTracking: 1002,
      anyMouseTracking: 1003,
      bracketedPaste: 2004,
      synchronizedOutput: 2026,
    });
  });
});
