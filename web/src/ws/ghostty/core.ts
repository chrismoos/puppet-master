import {
  type GhosttyKeyboardLayoutMap,
  ghosttyConsumedMods,
  ghosttyKeyForEvent,
  ghosttyUnshiftedCodepoint,
  loadGhosttyKeyboardLayoutMap,
} from "./keyCodes";
import { GhosttyRuntime, loadGhosttyRuntime } from "./runtime";
import {
  GHOSTTY_C_API_NULL_CONFIG,
  GHOSTTY_SNAPSHOT_DECODER_DATA,
  GHOSTTY_DEC_MODE,
  GHOSTTY_GRID_REF_TAG,
  GHOSTTY_KEY_EVENT_ACTION,
  GHOSTTY_MOUSE_ENCODER_OPTION,
  GHOSTTY_MOUSE_EVENT_ACTION,
  GHOSTTY_PALETTE_COLOR_BYTES,
  GHOSTTY_PALETTE_COLOR_COUNT,
  GHOSTTY_RENDER_STATE_DATA,
  GHOSTTY_RENDER_STATE_DIRTY,
  GHOSTTY_RENDER_STATE_ROW_GET,
  GHOSTTY_RENDER_STATE_ROW_SET,
  GHOSTTY_RENDER_STATE_SET,
  GHOSTTY_ROW_CELLS_QUERY_FIELD_COUNT,
  GHOSTTY_ROW_METADATA,
  GHOSTTY_SCROLL_VIEWPORT_TAG,
  GHOSTTY_TERMINAL_DATA,
  GHOSTTY_TERMINAL_DIMENSION,
  GHOSTTY_TERMINAL_OPTION,
  GHOSTTY_TERMINAL_SCREEN,
  type GhosttyPointTag,
} from "./abi";

const GHOSTTY_SUCCESS = 0;
const GHOSTTY_OUT_OF_SPACE = -3;
export const MAX_SCROLLBACK_BYTES = 32 * 1024 * 1024;
// wasm32 GhosttyTerminalSelectionFormatOptions layout at the pinned libghostty-vt revision.
const SELECTION_FORMAT_OPTIONS_SIZE = 16;
const PALETTE_BYTES = GHOSTTY_PALETTE_COLOR_COUNT * GHOSTTY_PALETTE_COLOR_BYTES;

const CELL_DATA = {
  raw: 1,
  style: 2,
  graphemesLength: 3,
  graphemes: 4,
  background: 5,
  foreground: 6,
  selected: 7,
} as const;

// Optional row_cells_get_multi colors must come last; an unset color stops the query.
const CELL_QUERY = {
  keys: 0,
  values: 20,
  graphemesLength: 40,
  selected: 44,
  background: 48,
  foreground: 52,
  written: 56,
  size: 60,
} as const;

const RAW_CELL_DATA = {
  wide: 3,
} as const;

export const GHOSTTY_CELL_WIDE = {
  narrow: 0,
  wide: 1,
  spacerTail: 2,
  spacerHead: 3,
} as const;

export interface GhosttyColor {
  readonly r: number;
  readonly g: number;
  readonly b: number;
}

export interface GhosttyTheme {
  readonly foreground: GhosttyColor;
  readonly background: GhosttyColor;
  readonly cursor: GhosttyColor;
  readonly palette?: readonly GhosttyColor[];
  readonly selectionBackground?: string;
}

export interface GhosttyCell {
  readonly text: string;
  readonly wide: number;
  readonly foreground: GhosttyColor;
  readonly background: GhosttyColor;
  readonly bold: boolean;
  readonly italic: boolean;
  readonly invisible: boolean;
  readonly strikethrough: boolean;
  readonly overline: boolean;
  readonly underline: boolean;
  readonly selected: boolean;
}

export interface GhosttyRow {
  readonly cells: readonly GhosttyCell[];
  readonly text: string;
  readonly isWrapContinuation: boolean;
  readonly wrapsToNext: boolean;
}

export interface GhosttySnapshot {
  readonly cols: number;
  readonly rows: number;
  readonly foreground: GhosttyColor;
  readonly background: GhosttyColor;
  readonly cursor: GhosttyColor;
  readonly cursorX: number;
  readonly cursorY: number;
  readonly cursorVisible: boolean;
  readonly cursorBlinking: boolean;
  readonly cursorStyle: number;
  readonly dirtyRows: ReadonlySet<number>;
  readonly rowData: readonly GhosttyRow[];
}

export interface GhosttySelectionRange {
  readonly viewport: {
    readonly start: { readonly x: number; readonly y: number };
    readonly end: { readonly x: number; readonly y: number };
  };
  readonly screen: {
    readonly start: { readonly x: number; readonly y: number };
    readonly end: { readonly x: number; readonly y: number };
  };
}

export interface GhosttyScrollbar {
  readonly total: number;
  readonly offset: number;
  readonly len: number;
}

export interface GhosttyPointInput {
  readonly x: number;
  readonly y: number;
  readonly tag?: GhosttyPointTag;
}

export interface GhosttyMouseInput {
  readonly action: "press" | "release" | "motion";
  readonly button: number | null;
  readonly mods: number;
  readonly x: number;
  readonly y: number;
  readonly screenWidth: number;
  readonly screenHeight: number;
  readonly cellWidth: number;
  readonly cellHeight: number;
  readonly paddingLeft: number;
  readonly paddingRight: number;
  readonly paddingTop: number;
  readonly paddingBottom: number;
  readonly anyButtonPressed: boolean;
}

export interface GhosttyCoreOptions {
  readonly maxScrollbackBytes?: number;
}

const decoder = new TextDecoder();
const encoder = new TextEncoder();

function blend(foreground: GhosttyColor, background: GhosttyColor): GhosttyColor {
  const channel = (front: number, back: number) => Math.floor((front * 155 + back * 100) / 255);
  return {
    r: channel(foreground.r, background.r),
    g: channel(foreground.g, background.g),
    b: channel(foreground.b, background.b),
  };
}

function sameColor(left: GhosttyColor, right: GhosttyColor): boolean {
  return left.r === right.r && left.g === right.g && left.b === right.b;
}

/** Bounds String.fromCodePoint arguments for cells with enormous combining-mark sequences. */
export function ghosttyCellText(codepointView: DataView, graphemeLength: number): string {
  if (graphemeLength === 1) return String.fromCodePoint(codepointView.getUint32(0, true));
  const CHUNK_SIZE = 4_096;
  let text = "";
  for (let start = 0; start < graphemeLength; start += CHUNK_SIZE) {
    const count = Math.min(CHUNK_SIZE, graphemeLength - start);
    const codes = new Array<number>(count);
    for (let index = 0; index < count; index += 1) {
      codes[index] = codepointView.getUint32((start + index) * 4, true);
    }
    text += String.fromCodePoint(...codes);
  }
  return text;
}

export class GhosttyTerminalCore {
  private readonly runtime: GhosttyRuntime;
  private terminalSlot = 0;
  private terminal = 0;
  private renderStateSlot = 0;
  private renderState = 0;
  private rowIteratorSlot = 0;
  private rowCellsSlot = 0;
  private keyEncoderSlot = 0;
  private keyEncoder = 0;
  private keyEventSlot = 0;
  private keyEvent = 0;
  private mouseEncoderSlot = 0;
  private mouseEncoder = 0;
  private mouseEventSlot = 0;
  private mouseEvent = 0;
  private ptyWriterId = 0;
  private ptyWriter: ((data: string) => void) | null = null;
  private scratch = 0;
  private graphemes = 0;
  private graphemeCapacity = 0;
  private style = 0;
  private cellQuery = 0;
  private scrollbar = 0;
  private rows: GhosttyRow[] = [];
  private disposed = false;
  private theme: GhosttyTheme | null = null;
  private keyboardLayoutMap: GhosttyKeyboardLayoutMap | undefined;

  private constructor(runtime: GhosttyRuntime) {
    this.runtime = runtime;
    void loadGhosttyKeyboardLayoutMap().then((layoutMap) => {
      if (!this.disposed) this.keyboardLayoutMap = layoutMap;
    });
  }

  static async create(
    cols: number,
    rows: number,
    cellWidth: number,
    cellHeight: number,
    theme: GhosttyTheme,
    onPtyData: (data: string) => void,
    options: GhosttyCoreOptions = {},
  ): Promise<GhosttyTerminalCore> {
    const core = new GhosttyTerminalCore(await loadGhosttyRuntime());
    try {
      core.initialize(cols, rows, cellWidth, cellHeight, theme, onPtyData, options);
      return core;
    } catch (error) {
      core.dispose();
      throw error;
    }
  }

  private initialize(
    cols: number,
    rows: number,
    cellWidth: number,
    cellHeight: number,
    theme: GhosttyTheme,
    onPtyData: (data: string) => void,
    options: GhosttyCoreOptions,
  ): void {
    this.terminalSlot = this.runtime.allocOpaque();
    this.assertSuccess(
      "ghostty_terminal_new",
      this.runtime.call("ghostty_terminal_new", GHOSTTY_C_API_NULL_CONFIG, this.terminalSlot, cols, rows),
    );
    this.terminal = this.runtime.readPointer(this.terminalSlot);
    const scrollback = this.runtime.alloc(4);
    this.runtime.view(scrollback, 4).setUint32(0, options.maxScrollbackBytes ?? MAX_SCROLLBACK_BYTES, true);
    this.assertSuccess("ghostty_terminal_set", this.runtime.call(
      "ghostty_terminal_set", this.terminal, GHOSTTY_TERMINAL_OPTION.scrollbackMaxBytes, scrollback,
    ));
    this.runtime.free(scrollback, 4);
    this.applyDefaultCursorBlink();
    this.ptyWriter = onPtyData;
    this.ptyWriterId = this.runtime.attachPtyWriter(this.terminal, onPtyData);

    this.renderStateSlot = this.runtime.allocOpaque();
    this.assertSuccess(
      "ghostty_render_state_new",
      this.runtime.call("ghostty_render_state_new", GHOSTTY_C_API_NULL_CONFIG, this.renderStateSlot),
    );
    this.renderState = this.runtime.readPointer(this.renderStateSlot);

    this.rowIteratorSlot = this.runtime.allocOpaque();
    this.assertSuccess(
      "ghostty_render_state_row_iterator_new",
      this.runtime.call("ghostty_render_state_row_iterator_new", GHOSTTY_C_API_NULL_CONFIG, this.rowIteratorSlot),
    );
    this.rowCellsSlot = this.runtime.allocOpaque();
    this.assertSuccess(
      "ghostty_render_state_row_cells_new",
      this.runtime.call("ghostty_render_state_row_cells_new", GHOSTTY_C_API_NULL_CONFIG, this.rowCellsSlot),
    );

    this.keyEncoderSlot = this.runtime.allocOpaque();
    this.assertSuccess(
      "ghostty_key_encoder_new",
      this.runtime.call("ghostty_key_encoder_new", GHOSTTY_C_API_NULL_CONFIG, this.keyEncoderSlot),
    );
    this.keyEncoder = this.runtime.readPointer(this.keyEncoderSlot);
    this.keyEventSlot = this.runtime.allocOpaque();
    this.assertSuccess(
      "ghostty_key_event_new",
      this.runtime.call("ghostty_key_event_new", GHOSTTY_C_API_NULL_CONFIG, this.keyEventSlot),
    );
    this.keyEvent = this.runtime.readPointer(this.keyEventSlot);

    this.mouseEncoderSlot = this.runtime.allocOpaque();
    this.assertSuccess(
      "ghostty_mouse_encoder_new",
      this.runtime.call("ghostty_mouse_encoder_new", GHOSTTY_C_API_NULL_CONFIG, this.mouseEncoderSlot),
    );
    this.mouseEncoder = this.runtime.readPointer(this.mouseEncoderSlot);
    this.mouseEventSlot = this.runtime.allocOpaque();
    this.assertSuccess(
      "ghostty_mouse_event_new",
      this.runtime.call("ghostty_mouse_event_new", GHOSTTY_C_API_NULL_CONFIG, this.mouseEventSlot),
    );
    this.mouseEvent = this.runtime.readPointer(this.mouseEventSlot);

    this.scratch = this.runtime.alloc(16);
    const styleSize = this.runtime.layout("GhosttyStyle").size;
    this.style = this.runtime.alloc(styleSize);
    this.runtime.setField(this.style, "GhosttyStyle", "size", styleSize);
    this.cellQuery = this.runtime.alloc(CELL_QUERY.size);
    const queryView = this.runtime.memoryDataView();
    const cellFields = [
      [CELL_DATA.style, this.style],
      [CELL_DATA.graphemesLength, this.cellQuery + CELL_QUERY.graphemesLength],
      [CELL_DATA.selected, this.cellQuery + CELL_QUERY.selected],
      [CELL_DATA.background, this.cellQuery + CELL_QUERY.background],
      [CELL_DATA.foreground, this.cellQuery + CELL_QUERY.foreground],
    ] as const;
    for (const [index, [key, pointer]] of cellFields.entries()) {
      queryView.setUint32(this.cellQuery + CELL_QUERY.keys + index * 4, key, true);
      queryView.setUint32(this.cellQuery + CELL_QUERY.values + index * 4, pointer, true);
    }
    this.scrollbar = this.runtime.alloc(this.runtime.layout("GhosttyTerminalScrollbar").size);
    this.setTheme(theme);
    this.resize(cols, rows, cellWidth, cellHeight);
  }

  write(data: string | Uint8Array, onSynchronizedOutputEnd?: () => void): void {
    this.ensureActive();
    const bytes = typeof data === "string" ? encoder.encode(data) : data;
    if (bytes.length === 0) return;
    const pointer = this.runtime.alloc(bytes.length);
    this.runtime.bytes(pointer, bytes.length).set(bytes);
    // An existing render deadline belongs to one group. Observe Ghostty before
    // the next escape can reopen it; Ghostty still parses OSC/DCS and split VT
    // sequences itself. Once that group ends, the remaining bytes can be batched.
    let synchronized = onSynchronizedOutputEnd !== undefined && this.isSynchronizedOutput();
    try {
      for (let start = 0; start < bytes.length;) {
        const nextEscape = synchronized ? bytes.indexOf(0x1b, start + 1) : -1;
        const end = nextEscape === -1 ? bytes.length : nextEscape;
        this.runtime.call("ghostty_terminal_vt_write", this.terminal, pointer + start, end - start);
        start = end;
        if (synchronized && !this.isSynchronizedOutput()) {
          synchronized = false;
          onSynchronizedOutputEnd?.();
        }
      }
    } finally {
      this.runtime.free(pointer, bytes.length);
    }
  }

  resetAndWrite(data: string | Uint8Array): void {
    this.ensureActive();
    this.runtime.call("ghostty_terminal_reset", this.terminal);
    // RIS restores the built-in steady cursor; reapply the embedder default before replay.
    this.applyDefaultCursorBlink();
    this.rows = [];
    if (data.length === 0) return;
    const writer = this.ptyWriter;
    if (this.ptyWriterId !== 0) {
      this.runtime.detachPtyWriter(this.terminal, this.ptyWriterId);
      this.ptyWriterId = 0;
    }
    try {
      this.write(data);
    } finally {
      if (writer !== null && !this.disposed) {
        this.ptyWriterId = this.runtime.attachPtyWriter(this.terminal, writer);
      }
    }
  }

  restoreSnapshot(data: Uint8Array): void {
    this.ensureActive();
    const input = this.runtime.alloc(data.length);
    const decoderSlot = this.runtime.allocOpaque();
    const terminalSlot = this.runtime.allocOpaque();
    let snapshotDecoder = 0;
    let restored = 0;
    try {
      this.runtime.bytes(input, data.length).set(data);
      this.assertSuccess("ghostty_snapshot_decoder_new_buf", this.runtime.call(
        "ghostty_snapshot_decoder_new_buf", 0, decoderSlot, input, data.length,
      ));
      snapshotDecoder = this.runtime.readPointer(decoderSlot);
      this.assertSuccess("ghostty_snapshot_decoder_decode", this.runtime.call(
        "ghostty_snapshot_decoder_decode", snapshotDecoder, terminalSlot,
      ));
      restored = this.runtime.readPointer(terminalSlot);
      this.assertSuccess("ghostty_snapshot_decoder_get(source offset)", this.runtime.call(
        "ghostty_snapshot_decoder_get", snapshotDecoder, GHOSTTY_SNAPSHOT_DECODER_DATA.sourceOffset, decoderSlot,
      ));
      if (this.runtime.readPointer(decoderSlot) !== data.length) {
        throw new Error("Ghostty snapshot contains trailing bytes");
      }
      if (this.ptyWriterId) this.runtime.detachPtyWriter(this.terminal, this.ptyWriterId);
      this.runtime.call("ghostty_render_state_row_cells_free", this.runtime.readPointer(this.rowCellsSlot));
      this.runtime.call("ghostty_render_state_row_iterator_free", this.runtime.readPointer(this.rowIteratorSlot));
      this.runtime.call("ghostty_render_state_free", this.renderState);
      this.runtime.call("ghostty_terminal_free", this.terminal);
      this.terminal = restored;
      restored = 0;
      this.runtime.call("ghostty_render_state_new", 0, this.renderStateSlot);
      this.renderState = this.runtime.readPointer(this.renderStateSlot);
      this.runtime.call("ghostty_render_state_row_iterator_new", 0, this.rowIteratorSlot);
      this.runtime.call("ghostty_render_state_row_cells_new", 0, this.rowCellsSlot);
      this.rows = [];
      this.ptyWriterId = this.ptyWriter ? this.runtime.attachPtyWriter(this.terminal, this.ptyWriter) : 0;
      if (this.theme) this.setTheme(this.theme);
    } finally {
      if (restored) this.runtime.call("ghostty_terminal_free", restored);
      if (snapshotDecoder) this.runtime.call("ghostty_snapshot_decoder_free", snapshotDecoder);
      this.runtime.freeOpaque(decoderSlot);
      this.runtime.freeOpaque(terminalSlot);
      this.runtime.free(input, data.length);
    }
  }

  resize(cols: number, rows: number, cellWidth: number, cellHeight: number): void {
    this.ensureActive();
    this.assertSuccess(
      "ghostty_terminal_resize",
      this.runtime.call(
        "ghostty_terminal_resize",
        this.terminal,
        Math.max(GHOSTTY_TERMINAL_DIMENSION.minimum, Math.min(GHOSTTY_TERMINAL_DIMENSION.maximum, cols)),
        Math.max(GHOSTTY_TERMINAL_DIMENSION.minimum, Math.min(GHOSTTY_TERMINAL_DIMENSION.maximum, rows)),
        Math.max(1, Math.round(cellWidth)),
        Math.max(1, Math.round(cellHeight)),
      ),
    );
  }

  // Option 23 sets blink on startup and DECSCUSR reset; explicit DECSCUSR or DEC mode 12 still wins.
  private applyDefaultCursorBlink(): void {
    const blink = this.runtime.alloc(1);
    this.runtime.bytes(blink, 1)[0] = 1;
    this.runtime.call("ghostty_terminal_set", this.terminal, GHOSTTY_TERMINAL_OPTION.defaultCursorBlink, blink);
    this.runtime.free(blink, 1);
  }

  setTheme(theme: GhosttyTheme): void {
    this.ensureActive();
    this.theme = theme;
    const color = this.runtime.alloc(3);
    for (const [option, value] of [
      [GHOSTTY_TERMINAL_OPTION.foreground, theme.foreground],
      [GHOSTTY_TERMINAL_OPTION.background, theme.background],
      [GHOSTTY_TERMINAL_OPTION.cursor, theme.cursor],
    ] as const) {
      this.runtime.bytes(color, 3).set([value.r, value.g, value.b]);
      this.runtime.call("ghostty_terminal_set", this.terminal, option, color);
    }
    this.runtime.free(color, 3);
    if (theme.palette) this.setPalette(theme.palette);
  }

  private setPalette(colors: readonly GhosttyColor[]): void {
    const palette = this.runtime.alloc(PALETTE_BYTES);
    this.runtime.call("ghostty_terminal_get", this.terminal, GHOSTTY_TERMINAL_DATA.defaultPalette, palette);
    const bytes = this.runtime.bytes(palette, PALETTE_BYTES);
    colors.slice(0, PALETTE_BYTES / 3).forEach((color, index) => {
      bytes.set([color.r, color.g, color.b], index * 3);
    });
    this.runtime.call("ghostty_terminal_set", this.terminal, GHOSTTY_TERMINAL_OPTION.palette, palette);
    this.runtime.free(palette, PALETTE_BYTES);
  }

  scroll(deltaRows: number): void {
    this.ensureActive();
    const layout = this.runtime.layout("GhosttyTerminalScrollViewport");
    const scroll = this.runtime.alloc(layout.size);
    this.runtime.setField(scroll, "GhosttyTerminalScrollViewport", "tag", GHOSTTY_SCROLL_VIEWPORT_TAG.delta);
    const value = layout.fields.value!;
    this.runtime.view(scroll + value.offset, value.size).setInt32(0, deltaRows, true);
    this.runtime.call("ghostty_terminal_scroll_viewport", this.terminal, scroll);
    this.runtime.free(scroll, layout.size);
  }

  scrollToBottom(): void {
    this.ensureActive();
    const layout = this.runtime.layout("GhosttyTerminalScrollViewport");
    const scroll = this.runtime.alloc(layout.size);
    this.runtime.setField(scroll, "GhosttyTerminalScrollViewport", "tag", GHOSTTY_SCROLL_VIEWPORT_TAG.bottom);
    this.runtime.call("ghostty_terminal_scroll_viewport", this.terminal, scroll);
    this.runtime.free(scroll, layout.size);
  }

  isViewportActive(): boolean {
    this.ensureActive();
    this.runtime.bytes(this.scratch, 1)[0] = 0;
    return (
      this.runtime.call("ghostty_terminal_get", this.terminal, GHOSTTY_TERMINAL_DATA.viewportActive, this.scratch) ===
        GHOSTTY_SUCCESS && this.runtime.bytes(this.scratch, 1)[0] !== 0
    );
  }

  scrollbarState(): GhosttyScrollbar | null {
    this.ensureActive();
    const layout = this.runtime.layout("GhosttyTerminalScrollbar");
    this.runtime.bytes(this.scrollbar, layout.size).fill(0);
    if (
      this.runtime.call("ghostty_terminal_get", this.terminal, GHOSTTY_TERMINAL_DATA.scrollbar, this.scrollbar) !==
      GHOSTTY_SUCCESS
    ) {
      return null;
    }
    return {
      total: this.runtime.readField(this.scrollbar, "GhosttyTerminalScrollbar", "total"),
      offset: this.runtime.readField(this.scrollbar, "GhosttyTerminalScrollbar", "offset"),
      len: this.runtime.readField(this.scrollbar, "GhosttyTerminalScrollbar", "len"),
    };
  }

  private modeEnabled(mode: number): boolean {
    const size = this.runtime.layout("GhosttyTerminalModeConfig").size;
    const config = this.runtime.alloc(size);
    try {
      this.runtime.setField(config, "GhosttyTerminalModeConfig", "mode", mode);
      this.assertSuccess("ghostty_terminal_get(mode)", this.runtime.call(
        "ghostty_terminal_get", this.terminal, GHOSTTY_TERMINAL_DATA.mode, config,
      ));
      return this.runtime.readField(config, "GhosttyTerminalModeConfig", "value") !== 0;
    } finally {
      this.runtime.free(config, size);
    }
  }

  isMouseTracking(): boolean {
    this.ensureActive();
    this.runtime.bytes(this.scratch, 1)[0] = 0;
    return (
      this.runtime.call("ghostty_terminal_get", this.terminal, GHOSTTY_TERMINAL_DATA.mouseTracking, this.scratch) ===
        GHOSTTY_SUCCESS && this.runtime.bytes(this.scratch, 1)[0] !== 0
    );
  }

  isMouseAnyEventTracking(): boolean {
    this.ensureActive();
    this.runtime.bytes(this.scratch, 1)[0] = 0;
    return (
      this.modeEnabled(GHOSTTY_DEC_MODE.anyMouseTracking)
    );
  }

  mouseTrackingMode(): "none" | "x10" | "vt200" | "drag" | "any" {
    this.ensureActive();
    const modes = [
      [GHOSTTY_DEC_MODE.anyMouseTracking, "any"],
      [GHOSTTY_DEC_MODE.buttonMouseTracking, "drag"],
      [GHOSTTY_DEC_MODE.normalMouseTracking, "vt200"],
      [GHOSTTY_DEC_MODE.x10MouseTracking, "x10"],
    ] as const;
    for (const [mode, name] of modes) {
      this.runtime.bytes(this.scratch, 1)[0] = 0;
      if (this.modeEnabled(mode)) return name;
    }
    return "none";
  }

  isSynchronizedOutput(): boolean {
    this.ensureActive();
    this.runtime.bytes(this.scratch, 1)[0] = 0;
    return (
      this.modeEnabled(GHOSTTY_DEC_MODE.synchronizedOutput)
    );
  }

  endSynchronizedOutput(): void {
    this.ensureActive();
    const config = this.runtime.alloc(this.runtime.layout("GhosttyTerminalModeConfig").size);
    this.runtime.setField(config, "GhosttyTerminalModeConfig", "mode", GHOSTTY_DEC_MODE.synchronizedOutput);
    this.runtime.setField(config, "GhosttyTerminalModeConfig", "value", 0);
    this.assertSuccess("ghostty_terminal_set(mode)", this.runtime.call(
      "ghostty_terminal_set", this.terminal, GHOSTTY_TERMINAL_OPTION.mode, config,
    ));
    this.runtime.free(config, this.runtime.layout("GhosttyTerminalModeConfig").size);
  }

  isAlternateScreen(): boolean {
    this.ensureActive();
    this.runtime.bytes(this.scratch, 4).fill(0);
    return (
      this.runtime.call("ghostty_terminal_get", this.terminal, GHOSTTY_TERMINAL_DATA.activeScreen, this.scratch) ===
        GHOSTTY_SUCCESS && this.runtime.view(this.scratch, 4).getUint32(0, true) === GHOSTTY_TERMINAL_SCREEN.alternate
    );
  }

  cursorPosition(): { x: number; y: number } {
    this.ensureActive();
    return {
      x: this.getTerminalU16(GHOSTTY_TERMINAL_DATA.cursorX),
      y: this.getTerminalU16(GHOSTTY_TERMINAL_DATA.cursorY),
    };
  }

  isApplicationCursorKeys(): boolean {
    this.ensureActive();
    this.runtime.bytes(this.scratch, 1)[0] = 0;
    return (
      this.modeEnabled(GHOSTTY_DEC_MODE.applicationCursorKeys)
    );
  }

  encodeKey(event: KeyboardEvent, action: "press" | "release" = "press"): string {
    this.ensureActive();
    this.runtime.call("ghostty_key_encoder_setopt_from_terminal", this.keyEncoder, this.terminal);
    this.runtime.call(
      "ghostty_key_event_set_action",
      this.keyEvent,
      action === "release" ? GHOSTTY_KEY_EVENT_ACTION.release : event.repeat ? GHOSTTY_KEY_EVENT_ACTION.repeat : GHOSTTY_KEY_EVENT_ACTION.press,
    );
    this.runtime.call("ghostty_key_event_set_key", this.keyEvent, ghosttyKeyForEvent(event));
    const mods =
      (event.shiftKey ? 1 : 0) |
      (event.ctrlKey ? 1 << 1 : 0) |
      (event.altKey ? 1 << 2 : 0) |
      (event.metaKey ? 1 << 3 : 0) |
      (event.getModifierState("CapsLock") ? 1 << 4 : 0) |
      (event.getModifierState("NumLock") ? 1 << 5 : 0);
    this.runtime.call("ghostty_key_event_set_mods", this.keyEvent, mods);
    this.runtime.call(
      "ghostty_key_event_set_consumed_mods",
      this.keyEvent,
      ghosttyConsumedMods(event),
    );
    this.runtime.call("ghostty_key_event_set_composing", this.keyEvent, event.isComposing ? 1 : 0);
    this.runtime.call(
      "ghostty_key_event_set_unshifted_codepoint",
      this.keyEvent,
      ghosttyUnshiftedCodepoint(event, this.keyboardLayoutMap),
    );

    const text = event.key.length === 1 ? event.key : "";
    const textBytes = encoder.encode(text);
    const textPointer = textBytes.length === 0 ? 0 : this.runtime.alloc(textBytes.length);
    if (textPointer !== 0) this.runtime.bytes(textPointer, textBytes.length).set(textBytes);
    this.runtime.call("ghostty_key_event_set_utf8", this.keyEvent, textPointer, textBytes.length);

    const written = this.runtime.alloc(4);
    const encoded = this.encodeOutput(written, (output, outputSize) =>
      this.runtime.call(
        "ghostty_key_encoder_encode",
        this.keyEncoder,
        this.keyEvent,
        output,
        outputSize,
        written,
      ),
    );
    this.runtime.free(written, 4);
    if (textPointer !== 0) this.runtime.free(textPointer, textBytes.length);
    return encoded;
  }

  encodePaste(data: string): string {
    this.ensureActive();
    const input = encoder.encode(data);
    if (input.length === 0) return "";
    const inputPointer = this.runtime.alloc(input.length);
    this.runtime.bytes(inputPointer, input.length).set(input);
    this.runtime.bytes(this.scratch, 1)[0] = 0;
    const bracketed =
      this.modeEnabled(GHOSTTY_DEC_MODE.bracketedPaste);
    const written = this.runtime.alloc(4);
    let encoded = "";
    const sizeResult = this.runtime.call(
      "ghostty_paste_encode",
      inputPointer,
      input.length,
      bracketed ? 1 : 0,
      0,
      0,
      written,
    );
    const outputSize = this.runtime.view(written, 4).getUint32(0, true);
    if (sizeResult === GHOSTTY_OUT_OF_SPACE && outputSize > 0) {
      const output = this.runtime.alloc(outputSize);
      const result = this.runtime.call(
        "ghostty_paste_encode",
        inputPointer,
        input.length,
        bracketed ? 1 : 0,
        output,
        outputSize,
        written,
      );
      const outputLength = this.runtime.view(written, 4).getUint32(0, true);
      encoded =
        result === GHOSTTY_SUCCESS ? decoder.decode(this.runtime.bytes(output, outputLength)) : "";
      this.runtime.free(output, outputSize);
    }
    this.runtime.free(written, 4);
    this.runtime.free(inputPointer, input.length);
    return encoded;
  }

  encodeMouse(input: GhosttyMouseInput): Uint8Array {
    this.ensureActive();
    this.runtime.call(
      "ghostty_mouse_encoder_setopt_from_terminal",
      this.mouseEncoder,
      this.terminal,
    );

    const sizeLayout = this.runtime.layout("GhosttyMouseEncoderSize");
    const size = this.runtime.alloc(sizeLayout.size);
    for (const [field, value] of [
      ["size", sizeLayout.size],
      ["screen_width", input.screenWidth],
      ["screen_height", input.screenHeight],
      ["cell_width", input.cellWidth],
      ["cell_height", input.cellHeight],
      ["padding_top", input.paddingTop],
      ["padding_bottom", input.paddingBottom],
      ["padding_right", input.paddingRight],
      ["padding_left", input.paddingLeft],
    ] as const) {
      this.runtime.setField(size, "GhosttyMouseEncoderSize", field, Math.max(0, Math.round(value)));
    }
    this.runtime.call("ghostty_mouse_encoder_setopt", this.mouseEncoder, GHOSTTY_MOUSE_ENCODER_OPTION.size, size);
    this.runtime.free(size, sizeLayout.size);

    this.runtime.bytes(this.scratch, 1)[0] = input.anyButtonPressed ? 1 : 0;
    this.runtime.call("ghostty_mouse_encoder_setopt", this.mouseEncoder, GHOSTTY_MOUSE_ENCODER_OPTION.anyButtonPressed, this.scratch);
    this.runtime.bytes(this.scratch, 1)[0] = 1;
    this.runtime.call("ghostty_mouse_encoder_setopt", this.mouseEncoder, GHOSTTY_MOUSE_ENCODER_OPTION.trackLastCell, this.scratch);

    this.runtime.call(
      "ghostty_mouse_event_set_action",
      this.mouseEvent,
      input.action === "press"
        ? GHOSTTY_MOUSE_EVENT_ACTION.press
        : input.action === "release"
          ? GHOSTTY_MOUSE_EVENT_ACTION.release
          : GHOSTTY_MOUSE_EVENT_ACTION.motion,
    );
    if (input.button === null) {
      this.runtime.call("ghostty_mouse_event_clear_button", this.mouseEvent);
    } else {
      this.runtime.call("ghostty_mouse_event_set_button", this.mouseEvent, input.button);
    }
    this.runtime.call("ghostty_mouse_event_set_mods", this.mouseEvent, input.mods);
    const positionLayout = this.runtime.layout("GhosttyMousePosition");
    const position = this.runtime.alloc(positionLayout.size);
    const positionView = this.runtime.view(position, positionLayout.size);
    positionView.setFloat32(positionLayout.fields.x!.offset, input.x, true);
    positionView.setFloat32(positionLayout.fields.y!.offset, input.y, true);
    this.runtime.call("ghostty_mouse_event_set_position", this.mouseEvent, position);
    this.runtime.free(position, positionLayout.size);

    const written = this.runtime.alloc(4);
    const encoded = this.encodeOutputBytes(written, (output, outputSize) =>
      this.runtime.call(
        "ghostty_mouse_encoder_encode",
        this.mouseEncoder,
        this.mouseEvent,
        output,
        outputSize,
        written,
      ),
    );
    this.runtime.free(written, 4);
    return encoded;
  }

  setSelection(anchor: GhosttyPointInput, end: GhosttyPointInput): void {
    this.ensureActive();
    const selectionLayout = this.runtime.layout("GhosttySelection");
    const gridRefSize = this.runtime.layout("GhosttyGridRef").size;
    const selection = this.runtime.alloc(selectionLayout.size);
    let start = 0;
    let endRef = 0;
    try {
      this.runtime.setField(selection, "GhosttySelection", "size", selectionLayout.size);
      start = this.gridRef(anchor.x, anchor.y, anchor.tag ?? GHOSTTY_GRID_REF_TAG.viewport);
      endRef = this.gridRef(end.x, end.y, end.tag ?? GHOSTTY_GRID_REF_TAG.viewport);
      const startField = selectionLayout.fields.start!;
      const endField = selectionLayout.fields.end!;
      this.runtime
        .bytes(selection + startField.offset, startField.size)
        .set(this.runtime.bytes(start, startField.size));
      this.runtime
        .bytes(selection + endField.offset, endField.size)
        .set(this.runtime.bytes(endRef, endField.size));
      this.runtime.call("ghostty_terminal_set", this.terminal, GHOSTTY_TERMINAL_OPTION.selection, selection);
    } finally {
      this.runtime.free(start, gridRefSize);
      this.runtime.free(endRef, gridRefSize);
      this.runtime.free(selection, selectionLayout.size);
    }
  }

  selectAll(): GhosttySelectionRange["screen"] | null {
    this.ensureActive();
    const layout = this.runtime.layout("GhosttySelection");
    const selection = this.runtime.alloc(layout.size);
    try {
      this.runtime.setField(selection, "GhosttySelection", "size", layout.size);
      if (
        this.runtime.call("ghostty_terminal_select_all", this.terminal, selection) !==
        GHOSTTY_SUCCESS
      ) {
        return null;
      }
      const start = this.pointFromGridRef(selection + layout.fields.start!.offset, GHOSTTY_GRID_REF_TAG.screen);
      const end = this.pointFromGridRef(selection + layout.fields.end!.offset, GHOSTTY_GRID_REF_TAG.screen);
      if (!start || !end) return null;
      this.runtime.call("ghostty_terminal_set", this.terminal, GHOSTTY_TERMINAL_OPTION.selection, selection);
      return { start, end };
    } finally {
      this.runtime.free(selection, layout.size);
    }
  }

  selectWord(col: number, row: number): GhosttySelectionRange | null {
    return this.selectAt(
      "GhosttyTerminalSelectWordOptions",
      "ghostty_terminal_select_word",
      col,
      row,
    );
  }

  selectLine(col: number, row: number): GhosttySelectionRange | null {
    return this.selectAt(
      "GhosttyTerminalSelectLineOptions",
      "ghostty_terminal_select_line",
      col,
      row,
    );
  }

  hyperlinkAt(col: number, row: number): string | null {
    this.ensureActive();
    const ref = this.gridRef(col, row);
    const written = this.runtime.alloc(4);
    const sizeResult = this.runtime.call("ghostty_grid_ref_hyperlink_uri", ref, 0, 0, written);
    const outputSize = this.runtime.view(written, 4).getUint32(0, true);
    let hyperlink: string | null = null;
    if (sizeResult === GHOSTTY_OUT_OF_SPACE && outputSize > 0) {
      const output = this.runtime.alloc(outputSize);
      const result = this.runtime.call(
        "ghostty_grid_ref_hyperlink_uri",
        ref,
        output,
        outputSize,
        written,
      );
      const outputLength = this.runtime.view(written, 4).getUint32(0, true);
      if (result === GHOSTTY_SUCCESS && outputLength > 0) {
        hyperlink = decoder.decode(this.runtime.bytes(output, outputLength));
      }
      this.runtime.free(output, outputSize);
    }
    this.runtime.free(written, 4);
    this.runtime.free(ref, this.runtime.layout("GhosttyGridRef").size);
    return hyperlink;
  }

  clearSelection(): void {
    this.ensureActive();
    this.runtime.call("ghostty_terminal_set", this.terminal, GHOSTTY_TERMINAL_OPTION.selection, 0);
  }

  snapshot(): GhosttySnapshot {
    this.ensureActive();
    this.assertSuccess(
      "ghostty_render_state_update",
      this.runtime.call("ghostty_render_state_update", this.renderState, this.terminal),
    );
    const cols = this.getU16(GHOSTTY_RENDER_STATE_DATA.cols);
    const rowCount = this.getU16(GHOSTTY_RENDER_STATE_DATA.rows);
    const dirty = this.getU32(GHOSTTY_RENDER_STATE_DATA.dirty);
    const foreground = this.getColor(GHOSTTY_RENDER_STATE_DATA.foreground, { r: 229, g: 231, b: 235 });
    const background = this.getColor(GHOSTTY_RENDER_STATE_DATA.background, { r: 0, g: 0, b: 0 });
    const cursorHasValue = this.getBool(GHOSTTY_RENDER_STATE_DATA.cursorHasValue);
    const cursor = cursorHasValue ? this.getColor(GHOSTTY_RENDER_STATE_DATA.cursor, foreground) : foreground;
    const cursorInViewport = this.getBool(GHOSTTY_RENDER_STATE_DATA.cursorInViewport);
    const cursorVisible = this.getBool(GHOSTTY_RENDER_STATE_DATA.cursorVisible) && cursorInViewport;
    const cursorX = cursorInViewport ? this.getU16(GHOSTTY_RENDER_STATE_DATA.cursorX) : -1;
    const cursorY = cursorInViewport ? this.getU16(GHOSTTY_RENDER_STATE_DATA.cursorY) : -1;

    if (this.rows.length !== rowCount || this.rows.some((row) => row.cells.length !== cols)) {
      this.rows = Array.from({ length: rowCount }, () => ({
        cells: Array.from({ length: cols }, () => this.emptyCell(foreground, background)),
        text: "",
        isWrapContinuation: false,
        wrapsToNext: false,
      }));
    }

    const dirtyRows = new Set<number>();
    if (dirty !== 0) {
      this.assertSuccess(
        "ghostty_render_state_get(row iterator)",
        this.runtime.call(
          "ghostty_render_state_get",
          this.renderState,
          GHOSTTY_RENDER_STATE_DATA.rowIterator,
          this.rowIteratorSlot,
        ),
      );
      const iterator = this.runtime.readPointer(this.rowIteratorSlot);
      let rowIndex = 0;
      while (
        rowIndex < rowCount &&
        this.runtime.call("ghostty_render_state_row_iterator_next", iterator) !== 0
      ) {
        const rowDirty = dirty === GHOSTTY_RENDER_STATE_DIRTY.rows || this.getRowBool(iterator, GHOSTTY_RENDER_STATE_ROW_GET.dirty);
        if (rowDirty) {
          this.rows[rowIndex] = this.readRow(iterator, cols, foreground, background);
          dirtyRows.add(rowIndex);
          this.runtime.bytes(this.scratch, 1)[0] = 0;
          this.runtime.call(
            "ghostty_render_state_row_set",
            iterator,
            GHOSTTY_RENDER_STATE_ROW_SET.clearDirty,
            this.scratch,
          );
        }
        rowIndex += 1;
      }
      this.runtime.view(this.scratch, 4).setUint32(0, 0, true);
      this.runtime.call(
        "ghostty_render_state_set",
        this.renderState,
        GHOSTTY_RENDER_STATE_SET.clearDirty,
        this.scratch,
      );
    }

    return {
      cols,
      rows: rowCount,
      foreground,
      background,
      cursor,
      cursorX,
      cursorY,
      cursorVisible,
      cursorBlinking: this.getBool(GHOSTTY_RENDER_STATE_DATA.cursorBlinking),
      cursorStyle: this.getU32(GHOSTTY_RENDER_STATE_DATA.cursorStyle),
      dirtyRows,
      rowData: this.rows,
    };
  }

  selectionText(): string {
    this.ensureActive();
    const options = this.runtime.alloc(SELECTION_FORMAT_OPTIONS_SIZE);
    const optionsView = this.runtime.view(options, SELECTION_FORMAT_OPTIONS_SIZE);
    optionsView.setUint32(0, SELECTION_FORMAT_OPTIONS_SIZE, true);
    optionsView.setUint32(4, 0, true);
    optionsView.setUint8(8, 1);
    optionsView.setUint8(9, 1);
    optionsView.setUint32(12, 0, true);
    const written = this.runtime.alloc(4);
    const sizeResult = this.runtime.call(
      "ghostty_terminal_selection_format_buf",
      this.terminal,
      options,
      0,
      0,
      written,
    );
    const outputSize = this.runtime.view(written, 4).getUint32(0, true);
    let text = "";
    if (sizeResult === GHOSTTY_OUT_OF_SPACE && outputSize > 0) {
      const output = this.runtime.alloc(outputSize);
      const result = this.runtime.call(
        "ghostty_terminal_selection_format_buf",
        this.terminal,
        options,
        output,
        outputSize,
        written,
      );
      const outputLength = this.runtime.view(written, 4).getUint32(0, true);
      if (result === GHOSTTY_SUCCESS) {
        text = decoder.decode(this.runtime.bytes(output, outputLength));
      }
      this.runtime.free(output, outputSize);
    }
    this.runtime.free(written, 4);
    this.runtime.free(options, SELECTION_FORMAT_OPTIONS_SIZE);
    return text;
  }

  viewportPointToScreen(col: number, row: number): { x: number; y: number } | null {
    return this.convertPoint(col, row, 1, 2);
  }

  screenPointToViewport(col: number, row: number): { x: number; y: number } | null {
    return this.convertPoint(col, row, 2, 1);
  }

  private convertPoint(
    col: number,
    row: number,
    fromTag: 1 | 2,
    toTag: 1 | 2,
  ): { x: number; y: number } | null {
    this.ensureActive();
    const ref = this.gridRef(col, row, fromTag);
    const point = this.pointFromGridRef(ref, toTag);
    this.runtime.free(ref, this.runtime.layout("GhosttyGridRef").size);
    return point;
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    if (this.mouseEvent) this.runtime.call("ghostty_mouse_event_free", this.mouseEvent);
    if (this.mouseEncoder) this.runtime.call("ghostty_mouse_encoder_free", this.mouseEncoder);
    if (this.keyEvent) this.runtime.call("ghostty_key_event_free", this.keyEvent);
    if (this.keyEncoder) this.runtime.call("ghostty_key_encoder_free", this.keyEncoder);
    if (this.rowCellsSlot) {
      const cells = this.runtime.readPointer(this.rowCellsSlot);
      if (cells) this.runtime.call("ghostty_render_state_row_cells_free", cells);
    }
    if (this.rowIteratorSlot) {
      const iterator = this.runtime.readPointer(this.rowIteratorSlot);
      if (iterator) this.runtime.call("ghostty_render_state_row_iterator_free", iterator);
    }
    if (this.renderState) this.runtime.call("ghostty_render_state_free", this.renderState);
    if (this.terminal) {
      if (this.ptyWriterId) this.runtime.detachPtyWriter(this.terminal, this.ptyWriterId);
      this.runtime.call("ghostty_terminal_free", this.terminal);
    }
    if (this.style) this.runtime.free(this.style, this.runtime.layout("GhosttyStyle").size);
    if (this.cellQuery) this.runtime.free(this.cellQuery, CELL_QUERY.size);
    if (this.scrollbar) {
      this.runtime.free(this.scrollbar, this.runtime.layout("GhosttyTerminalScrollbar").size);
    }
    if (this.scratch) this.runtime.free(this.scratch, 16);
    if (this.graphemes) this.runtime.free(this.graphemes, this.graphemeCapacity);
    for (const slot of [
      this.mouseEventSlot,
      this.mouseEncoderSlot,
      this.keyEventSlot,
      this.keyEncoderSlot,
      this.rowCellsSlot,
      this.rowIteratorSlot,
      this.renderStateSlot,
      this.terminalSlot,
    ]) {
      this.runtime.freeOpaque(slot);
    }
  }

  private encodeOutput(
    written: number,
    encode: (output: number, outputSize: number) => number,
  ): string {
    const sizeResult = encode(0, 0);
    const outputSize = this.runtime.view(written, 4).getUint32(0, true);
    if (sizeResult === GHOSTTY_SUCCESS && outputSize === 0) return "";
    if (sizeResult !== GHOSTTY_OUT_OF_SPACE || outputSize === 0) return "";

    const output = this.runtime.alloc(outputSize);
    const result = encode(output, outputSize);
    const outputLength = this.runtime.view(written, 4).getUint32(0, true);
    const encoded =
      result === GHOSTTY_SUCCESS ? decoder.decode(this.runtime.bytes(output, outputLength)) : "";
    this.runtime.free(output, outputSize);
    return encoded;
  }

  private encodeOutputBytes(
    written: number,
    encode: (output: number, outputSize: number) => number,
  ): Uint8Array {
    const sizeResult = encode(0, 0);
    const outputSize = this.runtime.view(written, 4).getUint32(0, true);
    if (sizeResult === GHOSTTY_SUCCESS && outputSize === 0) return new Uint8Array();
    if (sizeResult !== GHOSTTY_OUT_OF_SPACE || outputSize === 0) return new Uint8Array();

    const output = this.runtime.alloc(outputSize);
    const result = encode(output, outputSize);
    const outputLength = this.runtime.view(written, 4).getUint32(0, true);
    const encoded = result === GHOSTTY_SUCCESS
      ? this.runtime.bytes(output, outputLength).slice()
      : new Uint8Array();
    this.runtime.free(output, outputSize);
    return encoded;
  }

  private readRow(
    iterator: number,
    cols: number,
    defaultForeground: GhosttyColor,
    defaultBackground: GhosttyColor,
  ): GhosttyRow {
    this.assertSuccess(
      "ghostty_render_state_row_get(raw)",
      this.runtime.call("ghostty_render_state_row_get", iterator, GHOSTTY_RENDER_STATE_ROW_GET.raw, this.scratch),
    );
    const rawRow = this.runtime.view(this.scratch, 8).getBigUint64(0, true);
    this.runtime.bytes(this.scratch + 8, 1)[0] = 0;
    this.assertSuccess(
      "ghostty_row_get(wrap continuation)",
      this.runtime.call("ghostty_row_get", rawRow, GHOSTTY_ROW_METADATA.isWrapContinuation, this.scratch + 8),
    );
    const isWrapContinuation = this.runtime.bytes(this.scratch + 8, 1)[0] !== 0;
    this.runtime.bytes(this.scratch + 8, 1)[0] = 0;
    this.assertSuccess(
      "ghostty_row_get(wrap)",
      this.runtime.call("ghostty_row_get", rawRow, GHOSTTY_ROW_METADATA.wrapsToNext, this.scratch + 8),
    );
    const wrapsToNext = this.runtime.bytes(this.scratch + 8, 1)[0] !== 0;

    this.assertSuccess(
      "ghostty_render_state_row_get(cells)",
      this.runtime.call(
        "ghostty_render_state_row_get",
        iterator,
        GHOSTTY_RENDER_STATE_ROW_GET.cells,
        this.rowCellsSlot,
      ),
    );
    const cellsIterator = this.runtime.readPointer(this.rowCellsSlot);
    const { size: styleSize, fields: styleFields } = this.runtime.layout("GhosttyStyle");
    const cells: GhosttyCell[] = [];
    while (
      cells.length < cols &&
      this.runtime.call("ghostty_render_state_row_cells_next", cellsIterator) !== 0
    ) {
      this.runtime.bytes(this.style, styleSize).fill(0);
      this.runtime.setField(this.style, "GhosttyStyle", "size", styleSize);
      const result = this.runtime.call(
        "ghostty_render_state_row_cells_get_multi",
        cellsIterator,
        GHOSTTY_ROW_CELLS_QUERY_FIELD_COUNT,
        this.cellQuery + CELL_QUERY.keys,
        this.cellQuery + CELL_QUERY.values,
        this.cellQuery + CELL_QUERY.written,
      );
      const queryView = this.runtime.memoryDataView();
      const written = queryView.getUint32(this.cellQuery + CELL_QUERY.written, true);
      if (written < 3) this.assertSuccess("ghostty_render_state_row_cells_get_multi", result);
      const graphemeLength = queryView.getUint32(this.cellQuery + CELL_QUERY.graphemesLength, true);
      const selected = queryView.getUint8(this.cellQuery + CELL_QUERY.selected) !== 0;
      let background =
        written >= 4 ? this.readColor(this.cellQuery + CELL_QUERY.background) : defaultBackground;
      let foreground =
        written === 5
          ? this.readColor(this.cellQuery + CELL_QUERY.foreground)
          : written === 3
            ? this.getCellColor(cellsIterator, CELL_DATA.foreground, defaultForeground)
            : defaultForeground;
      let text = "";
      if (graphemeLength > 0) {
        const bufferSize = graphemeLength * 4;
        if (bufferSize > this.graphemeCapacity) {
          const capacity = Math.max(bufferSize, this.graphemeCapacity * 2);
          const buffer = this.runtime.alloc(capacity);
          this.runtime.free(this.graphemes, this.graphemeCapacity);
          this.graphemes = buffer;
          this.graphemeCapacity = capacity;
        }
        if (
          this.runtime.call(
            "ghostty_render_state_row_cells_get",
            cellsIterator,
            CELL_DATA.graphemes,
            this.graphemes,
          ) === GHOSTTY_SUCCESS
        ) {
          // Byte allocations need not be 4-byte aligned; use DataView instead of Uint32Array.
          const codepointView = this.runtime.view(this.graphemes, bufferSize);
          text = ghosttyCellText(codepointView, graphemeLength);
        }
      }
      let wide = 0;
      // A wrap spacer in the final column can follow a spacer tail.
      if (text.length === 0 && (cells.at(-1)?.text.length || cells.length === cols - 1)) {
        this.assertSuccess(
          "ghostty_render_state_row_cells_get(raw)",
          this.runtime.call(
            "ghostty_render_state_row_cells_get",
            cellsIterator,
            CELL_DATA.raw,
            this.scratch,
          ),
        );
        const rawCell = this.runtime.view(this.scratch, 8).getBigUint64(0, true);
        this.runtime.view(this.scratch + 8, 4).setUint32(0, 0, true);
        this.assertSuccess(
          "ghostty_cell_get(wide)",
          this.runtime.call("ghostty_cell_get", rawCell, RAW_CELL_DATA.wide, this.scratch + 8),
        );
        wide = this.runtime.view(this.scratch + 8, 4).getUint32(0, true);
      }
      // Read the style after allocation and ABI calls, which can grow WASM memory.
      const styleView = this.runtime.memoryDataView();
      if (styleView.getUint8(this.style + styleFields.inverse!.offset) !== 0) {
        [foreground, background] = [background, foreground];
      }
      if (styleView.getUint8(this.style + styleFields.faint!.offset) !== 0) {
        foreground = blend(foreground, background);
      }
      cells.push({
        text,
        wide,
        foreground,
        background,
        bold: styleView.getUint8(this.style + styleFields.bold!.offset) !== 0,
        italic: styleView.getUint8(this.style + styleFields.italic!.offset) !== 0,
        invisible: styleView.getUint8(this.style + styleFields.invisible!.offset) !== 0,
        strikethrough: styleView.getUint8(this.style + styleFields.strikethrough!.offset) !== 0,
        overline: styleView.getUint8(this.style + styleFields.overline!.offset) !== 0,
        underline: styleView.getInt32(this.style + styleFields.underline!.offset, true) !== 0,
        selected,
      });
    }
    while (cells.length < cols) cells.push(this.emptyCell(defaultForeground, defaultBackground));
    return {
      cells,
      text: cells
        .map((cell) => cell.text || " ")
        .join("")
        .trimEnd(),
      isWrapContinuation,
      wrapsToNext,
    };
  }

  private gridRef(
    col: number,
    row: number,
    tag: GhosttyPointTag = GHOSTTY_GRID_REF_TAG.viewport,
  ): number {
    const pointLayout = this.runtime.layout("GhosttyPoint");
    const point = this.runtime.alloc(pointLayout.size);
    this.runtime.setField(point, "GhosttyPoint", "tag", tag);
    const pointValue = pointLayout.fields.value!;
    const coordinate = point + pointValue.offset;
    this.runtime.setField(coordinate, "GhosttyPointCoordinate", "x", Math.max(0, col));
    this.runtime.setField(coordinate, "GhosttyPointCoordinate", "y", Math.max(0, row));
    const gridRefSize = this.runtime.layout("GhosttyGridRef").size;
    const gridRef = this.runtime.alloc(gridRefSize);
    this.runtime.setField(gridRef, "GhosttyGridRef", "size", gridRefSize);
    const result = this.runtime.call("ghostty_terminal_grid_ref", this.terminal, point, gridRef);
    this.runtime.free(point, pointLayout.size);
    if (result !== GHOSTTY_SUCCESS) {
      this.runtime.free(gridRef, gridRefSize);
      this.assertSuccess("ghostty_terminal_grid_ref", result);
    }
    return gridRef;
  }

  private selectAt(
    optionsName: "GhosttyTerminalSelectWordOptions" | "GhosttyTerminalSelectLineOptions",
    operation: "ghostty_terminal_select_word" | "ghostty_terminal_select_line",
    col: number,
    row: number,
  ): GhosttySelectionRange | null {
    this.ensureActive();
    const optionsLayout = this.runtime.layout(optionsName);
    const selectionLayout = this.runtime.layout("GhosttySelection");
    const options = this.runtime.alloc(optionsLayout.size);
    let ref = 0;
    let selection = 0;
    let range: GhosttySelectionRange | null = null;
    try {
      this.runtime.setField(options, optionsName, "size", optionsLayout.size);
      ref = this.gridRef(col, row);
      const refField = optionsLayout.fields.ref!;
      this.runtime
        .bytes(options + refField.offset, refField.size)
        .set(this.runtime.bytes(ref, refField.size));
      selection = this.runtime.alloc(selectionLayout.size);
      this.runtime.setField(selection, "GhosttySelection", "size", selectionLayout.size);
      const result = this.runtime.call(operation, this.terminal, options, selection);
      if (result === GHOSTTY_SUCCESS) {
        const start = selection + selectionLayout.fields.start!.offset;
        const end = selection + selectionLayout.fields.end!.offset;
        const viewportStart = this.pointFromGridRef(start, GHOSTTY_GRID_REF_TAG.viewport);
        const viewportEnd = this.pointFromGridRef(end, GHOSTTY_GRID_REF_TAG.viewport);
        const screenStart = this.pointFromGridRef(start, GHOSTTY_GRID_REF_TAG.screen);
        const screenEnd = this.pointFromGridRef(end, GHOSTTY_GRID_REF_TAG.screen);
        if (viewportStart && viewportEnd && screenStart && screenEnd) {
          range = {
            viewport: { start: viewportStart, end: viewportEnd },
            screen: { start: screenStart, end: screenEnd },
          };
        }
        this.runtime.call("ghostty_terminal_set", this.terminal, GHOSTTY_TERMINAL_OPTION.selection, selection);
      }
    } finally {
      this.runtime.free(selection, selectionLayout.size);
      this.runtime.free(ref, this.runtime.layout("GhosttyGridRef").size);
      this.runtime.free(options, optionsLayout.size);
    }
    return range;
  }

  private pointFromGridRef(ref: number, tag: GhosttyPointTag): { x: number; y: number } | null {
    const coordinateLayout = this.runtime.layout("GhosttyPointCoordinate");
    const coordinate = this.runtime.alloc(coordinateLayout.size);
    const result = this.runtime.call(
      "ghostty_terminal_point_from_grid_ref",
      this.terminal,
      ref,
      tag,
      coordinate,
    );
    const point =
      result === GHOSTTY_SUCCESS
        ? {
            x: this.runtime.readField(coordinate, "GhosttyPointCoordinate", "x"),
            y: this.runtime.readField(coordinate, "GhosttyPointCoordinate", "y"),
          }
        : null;
    this.runtime.free(coordinate, coordinateLayout.size);
    return point;
  }

  private getU16(data: number): number {
    this.runtime.bytes(this.scratch, 2).fill(0);
    this.assertSuccess(
      "ghostty_render_state_get",
      this.runtime.call("ghostty_render_state_get", this.renderState, data, this.scratch),
    );
    return this.runtime.view(this.scratch, 2).getUint16(0, true);
  }

  private getTerminalU16(data: number): number {
    this.runtime.bytes(this.scratch, 2).fill(0);
    this.assertSuccess(
      "ghostty_terminal_get",
      this.runtime.call("ghostty_terminal_get", this.terminal, data, this.scratch),
    );
    return this.runtime.view(this.scratch, 2).getUint16(0, true);
  }

  private getU32(data: number): number {
    this.runtime.bytes(this.scratch, 4).fill(0);
    this.assertSuccess(
      "ghostty_render_state_get",
      this.runtime.call("ghostty_render_state_get", this.renderState, data, this.scratch),
    );
    return this.runtime.view(this.scratch, 4).getUint32(0, true);
  }

  private getBool(data: number): boolean {
    this.runtime.bytes(this.scratch, 1)[0] = 0;
    this.assertSuccess(
      "ghostty_render_state_get",
      this.runtime.call("ghostty_render_state_get", this.renderState, data, this.scratch),
    );
    return this.runtime.bytes(this.scratch, 1)[0] !== 0;
  }

  private getColor(data: number, fallback: GhosttyColor): GhosttyColor {
    this.runtime.bytes(this.scratch, 3).fill(0);
    const result = this.runtime.call(
      "ghostty_render_state_get",
      this.renderState,
      data,
      this.scratch,
    );
    return result === GHOSTTY_SUCCESS ? this.readColor(this.scratch) : fallback;
  }

  private getRowBool(iterator: number, data: number): boolean {
    this.runtime.bytes(this.scratch, 1)[0] = 0;
    return (
      this.runtime.call("ghostty_render_state_row_get", iterator, data, this.scratch) ===
        GHOSTTY_SUCCESS && this.runtime.bytes(this.scratch, 1)[0] !== 0
    );
  }

  private getCellColor(iterator: number, data: number, fallback: GhosttyColor): GhosttyColor {
    this.runtime.bytes(this.scratch, 3).fill(0);
    const result = this.runtime.call(
      "ghostty_render_state_row_cells_get",
      iterator,
      data,
      this.scratch,
    );
    return result === GHOSTTY_SUCCESS ? this.readColor(this.scratch) : fallback;
  }

  private readColor(pointer: number): GhosttyColor {
    const view = this.runtime.memoryDataView();
    return {
      r: view.getUint8(pointer),
      g: view.getUint8(pointer + 1),
      b: view.getUint8(pointer + 2),
    };
  }

  private emptyCell(foreground: GhosttyColor, background: GhosttyColor): GhosttyCell {
    return {
      text: "",
      wide: 0,
      foreground,
      background,
      bold: false,
      italic: false,
      invisible: false,
      strikethrough: false,
      overline: false,
      underline: false,
      selected: false,
    };
  }

  private assertSuccess(operation: string, result: number): void {
    if (result !== GHOSTTY_SUCCESS) throw new Error(`${operation} failed with result ${result}`);
  }

  private ensureActive(): void {
    if (this.disposed) throw new Error("libghostty-vt terminal has been disposed");
  }
}

export function ghosttyColorsEqual(left: GhosttyColor, right: GhosttyColor): boolean {
  return sameColor(left, right);
}
