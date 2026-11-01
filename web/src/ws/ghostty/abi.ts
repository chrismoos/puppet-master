export const GHOSTTY_TERMINAL_OPTION = {
  foreground: 11,
  background: 12,
  cursor: 13,
  palette: 14,
  selection: 21,
  defaultCursorBlink: 23,
  scrollbackMaxBytes: 27,
  scrollbackMaxLines: 28,
  mode: 30,
} as const;

export const GHOSTTY_TERMINAL_DATA = {
  activeScreen: 6,
  scrollbar: 9,
  mouseTracking: 11,
  cursorX: 3,
  cursorY: 4,
  viewportActive: 32,
  mode: 37,
  defaultPalette: 25,
} as const;

export const GHOSTTY_DEC_MODE = {
  applicationCursorKeys: 1,
  x10MouseTracking: 9,
  normalMouseTracking: 1000,
  buttonMouseTracking: 1002,
  anyMouseTracking: 1003,
  bracketedPaste: 2004,
  synchronizedOutput: 2026,
} as const;

export const GHOSTTY_SCROLL_VIEWPORT_TAG = {
  top: 0,
  bottom: 1,
  delta: 2,
} as const;

export const GHOSTTY_RENDER_STATE_SET = {
  clearDirty: 0,
} as const;

export const GHOSTTY_RENDER_STATE_ROW_SET = {
  clearDirty: 0,
} as const;

export const GHOSTTY_RENDER_STATE_DIRTY = {
  rows: 2,
} as const;

export const GHOSTTY_RENDER_STATE_ROW_GET = {
  dirty: 1,
  raw: 2,
  cells: 3,
} as const;

export const GHOSTTY_RENDER_STATE_DATA = {
  cols: 1,
  rows: 2,
  dirty: 3,
  rowIterator: 4,
  background: 5,
  foreground: 6,
  cursor: 7,
  cursorHasValue: 8,
  cursorStyle: 10,
  cursorVisible: 11,
  cursorBlinking: 12,
  cursorInViewport: 14,
  cursorX: 15,
  cursorY: 16,
} as const;

export const GHOSTTY_ROW_METADATA = {
  wrapsToNext: 1,
  isWrapContinuation: 2,
} as const;

export const GHOSTTY_MOUSE_EVENT_ACTION = {
  press: 0,
  release: 1,
  motion: 2,
} as const;

export const GHOSTTY_KEY_EVENT_ACTION = {
  release: 0,
  press: 1,
  repeat: 2,
} as const;

export const GHOSTTY_MOUSE_EVENT_BUTTON = {
  left: 1,
  right: 2,
  middle: 3,
} as const;

export const GHOSTTY_TERMINAL_SCREEN = {
  primary: 0,
  alternate: 1,
} as const;

export const GHOSTTY_CURSOR_STYLE = {
  bar: 0,
  block: 1,
  underline: 2,
  hollowBlock: 3,
} as const;

export const GHOSTTY_GRID_REF_TAG = {
  viewport: 1,
  screen: 2,
} as const;

export const GHOSTTY_C_API_NULL_CONFIG = 0;
export const GHOSTTY_TERMINAL_DIMENSION = {
  minimum: 1,
  maximum: 65_535,
} as const;
export const GHOSTTY_PALETTE_COLOR_COUNT = 256;
export const GHOSTTY_PALETTE_COLOR_BYTES = 3;
export const GHOSTTY_ROW_CELLS_QUERY_FIELD_COUNT = 5;

export const GHOSTTY_MOUSE_ENCODER_OPTION = {
  size: 2,
  anyButtonPressed: 3,
  trackLastCell: 4,
} as const;

export type GhosttyPointTag = (typeof GHOSTTY_GRID_REF_TAG)[keyof typeof GHOSTTY_GRID_REF_TAG];

export const GHOSTTY_SNAPSHOT_DECODER_DATA = {
  sourceOffset: 2,
} as const;
