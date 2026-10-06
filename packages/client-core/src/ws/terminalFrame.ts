export const TERMINAL_SUBPROTOCOL = "pm-terminal-v1";

export const TERMINAL_TAG_OUTPUT = 0x01;
export const TERMINAL_TAG_INPUT = 0x02;
export const TERMINAL_TAG_RESIZE = 0x03;
export const TERMINAL_TAG_INPUT_SUBMIT = 0x04;
/** Output bytes the viewer has parsed, which paces the relay's flood to this viewer. */
export const TERMINAL_TAG_ACK = 0x05;
/** Asks for a fresh snapshot on the open socket, used after a width change. */
export const TERMINAL_TAG_RESYNC = 0x06;

export const TERMINAL_FLAG_REPLAY = 0x01;
export const TERMINAL_FLAG_REPLAY_START = 0x02;
export const TERMINAL_FLAG_REPLAY_SNAPSHOT = 0x08;
export const TERMINAL_FLAG_REPLAY_END = 0x04;

const GENERATION_OFFSET = 1;
const PAYLOAD_OFFSET = 10;
const RESIZE_COLS_OFFSET = 9;
const RESIZE_ROWS_OFFSET = 11;
const RESIZE_FRAME_BYTES = 13;

export interface TerminalOutputFrame {
  generation: bigint;
  flags: number;
  data: Uint8Array;
}

export interface TerminalResizeFrame {
  generation: bigint;
  cols: number;
  rows: number;
}

export function decodeTerminalOutput(frame: ArrayBuffer): TerminalOutputFrame | null {
  if (frame.byteLength < PAYLOAD_OFFSET) return null;
  const view = new DataView(frame);
  if (view.getUint8(0) !== TERMINAL_TAG_OUTPUT) return null;
  return {
    generation: view.getBigUint64(GENERATION_OFFSET, true),
    flags: view.getUint8(9),
    data: new Uint8Array(frame, PAYLOAD_OFFSET),
  };
}

export function encodeTerminalInput(generation: bigint, data: Uint8Array, submitted = false): ArrayBuffer {
  const frame = new Uint8Array(9 + data.byteLength);
  const view = new DataView(frame.buffer);
  view.setUint8(0, submitted ? TERMINAL_TAG_INPUT_SUBMIT : TERMINAL_TAG_INPUT);
  view.setBigUint64(GENERATION_OFFSET, generation, true);
  frame.set(data, 9);
  return frame.buffer;
}

export function encodeTerminalResize(generation: bigint, cols: number, rows: number): ArrayBuffer {
  const frame = new ArrayBuffer(RESIZE_FRAME_BYTES);
  const view = new DataView(frame);
  view.setUint8(0, TERMINAL_TAG_RESIZE);
  view.setBigUint64(GENERATION_OFFSET, generation, true);
  view.setUint16(RESIZE_COLS_OFFSET, cols, true);
  view.setUint16(RESIZE_ROWS_OFFSET, rows, true);
  return frame;
}

/** Decodes a daemon PTY size echo, the same frame layout the client
 * sends to resize. */
export function decodeTerminalResize(frame: ArrayBuffer): TerminalResizeFrame | null {
  if (frame.byteLength !== RESIZE_FRAME_BYTES) return null;
  const view = new DataView(frame);
  if (view.getUint8(0) !== TERMINAL_TAG_RESIZE) return null;
  return {
    generation: view.getBigUint64(GENERATION_OFFSET, true),
    cols: view.getUint16(RESIZE_COLS_OFFSET, true),
    rows: view.getUint16(RESIZE_ROWS_OFFSET, true),
  };
}

const ACK_FRAME_BYTES = 13;

export function encodeTerminalAck(generation: bigint, bytes: number): ArrayBuffer {
  const frame = new ArrayBuffer(ACK_FRAME_BYTES);
  const view = new DataView(frame);
  view.setUint8(0, TERMINAL_TAG_ACK);
  view.setBigUint64(GENERATION_OFFSET, generation, true);
  view.setUint32(9, bytes, true);
  return frame;
}

export function encodeTerminalResync(generation: bigint): ArrayBuffer {
  const frame = new ArrayBuffer(9);
  const view = new DataView(frame);
  view.setUint8(0, TERMINAL_TAG_RESYNC);
  view.setBigUint64(GENERATION_OFFSET, generation, true);
  return frame;
}

export const TERMINAL_TAG_OWNERSHIP = 0x07;
export const TERMINAL_TAG_RESIZE_REQUEST = 0x08;
const REVISION_OFFSET = GENERATION_OFFSET + 8;
const OWNER_OFFSET = REVISION_OFFSET + 8;
const OWNERSHIP_ACK_OFFSET = OWNER_OFFSET + 8;
const OWNERSHIP_COLS_OFFSET = OWNERSHIP_ACK_OFFSET + 8;
const OWNERSHIP_ROWS_OFFSET = OWNERSHIP_COLS_OFFSET + 2;
const OWNERSHIP_FRAME_BYTES = OWNERSHIP_ROWS_OFFSET + 2;
const REQUEST_OFFSET = GENERATION_OFFSET + 8;
const REQUEST_COLS_OFFSET = REQUEST_OFFSET + 8;
const REQUEST_ROWS_OFFSET = REQUEST_COLS_OFFSET + 2;
const REQUEST_FRAME_BYTES = REQUEST_ROWS_OFFSET + 2;

export interface TerminalOwnershipFrame {
  generation: bigint;
  revision: bigint;
  owner: bigint;
  acknowledgment: bigint;
  cols: number;
  rows: number;
}

export function decodeTerminalOwnership(frame: ArrayBuffer): TerminalOwnershipFrame | null {
  if (frame.byteLength !== OWNERSHIP_FRAME_BYTES) return null;
  const view = new DataView(frame);
  if (view.getUint8(0) !== TERMINAL_TAG_OWNERSHIP) return null;
  return {
    generation: view.getBigUint64(GENERATION_OFFSET, true),
    revision: view.getBigUint64(REVISION_OFFSET, true),
    owner: view.getBigUint64(OWNER_OFFSET, true),
    acknowledgment: view.getBigUint64(OWNERSHIP_ACK_OFFSET, true),
    cols: view.getUint16(OWNERSHIP_COLS_OFFSET, true),
    rows: view.getUint16(OWNERSHIP_ROWS_OFFSET, true),
  };
}

export function encodeTerminalResizeRequest(generation: bigint, request: bigint, cols: number, rows: number): ArrayBuffer {
  const frame = new ArrayBuffer(REQUEST_FRAME_BYTES);
  const view = new DataView(frame);
  view.setUint8(0, TERMINAL_TAG_RESIZE_REQUEST);
  view.setBigUint64(GENERATION_OFFSET, generation, true);
  view.setBigUint64(REQUEST_OFFSET, request, true);
  view.setUint16(REQUEST_COLS_OFFSET, cols, true);
  view.setUint16(REQUEST_ROWS_OFFSET, rows, true);
  return frame;
}

export function encodeTerminalOwnership(state: TerminalOwnershipFrame): ArrayBuffer {
  const frame = new ArrayBuffer(OWNERSHIP_FRAME_BYTES);
  const view = new DataView(frame);
  view.setUint8(0, TERMINAL_TAG_OWNERSHIP);
  view.setBigUint64(GENERATION_OFFSET, state.generation, true);
  view.setBigUint64(REVISION_OFFSET, state.revision, true);
  view.setBigUint64(OWNER_OFFSET, state.owner, true);
  view.setBigUint64(OWNERSHIP_ACK_OFFSET, state.acknowledgment, true);
  view.setUint16(OWNERSHIP_COLS_OFFSET, state.cols, true);
  view.setUint16(OWNERSHIP_ROWS_OFFSET, state.rows, true);
  return frame;
}
