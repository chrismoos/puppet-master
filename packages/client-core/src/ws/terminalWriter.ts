import type { PtyFrame } from "./pty";

/** RIS: drops the screen, the saved lines, and every mode. */
export const RESET_SEQUENCE = new Uint8Array([0x1b, 0x63]);
export const SYNC_OUTPUT_START = new Uint8Array([0x1b, 0x5b, 0x3f, 0x32, 0x30, 0x32, 0x36, 0x68]);
const SYNC_OUTPUT_END = new Uint8Array([0x1b, 0x5b, 0x3f, 0x32, 0x30, 0x32, 0x36, 0x6c]);
export const REPLAY_WRITE_CHUNK_BYTES = 32 * 1024;

/** The write surface of an xterm-style terminal emulator. */
export interface TerminalWriteTarget {
  write(data: Uint8Array, callback?: () => void): void;
}

function replayWriteChunks(data: Uint8Array): Uint8Array[] {
  const sourceChunks = data.byteLength === 0
    ? [data]
    : Array.from(
      { length: Math.ceil(data.byteLength / REPLAY_WRITE_CHUNK_BYTES) },
      (_, index) => data.subarray(
        index * REPLAY_WRITE_CHUNK_BYTES,
        Math.min(data.byteLength, (index + 1) * REPLAY_WRITE_CHUNK_BYTES),
      ),
    );
  return sourceChunks.map((chunk, index) => {
    const prefixBytes = index === 0 ? RESET_SEQUENCE.byteLength + SYNC_OUTPUT_START.byteLength : 0;
    const suffixBytes = index === sourceChunks.length - 1 ? SYNC_OUTPUT_END.byteLength : 0;
    const write = new Uint8Array(prefixBytes + chunk.byteLength + suffixBytes);
    if (index === 0) {
      write.set(RESET_SEQUENCE);
      write.set(SYNC_OUTPUT_START, RESET_SEQUENCE.byteLength);
    }
    write.set(chunk, prefixBytes);
    if (suffixBytes > 0) write.set(SYNC_OUTPUT_END, prefixBytes + chunk.byteLength);
    return write;
  });
}

export function writeTerminalFrame<Viewport>(
  terminal: TerminalWriteTarget,
  frame: PtyFrame,
  viewport: Viewport,
  restoreViewport: (viewport: Viewport) => void,
  onParsed?: (viewport: Viewport) => void,
  shouldRestore: () => boolean = () => true,
): void {
  if (!frame.replay) {
    terminal.write(frame.data, () => {
      if (shouldRestore()) restoreViewport(viewport);
      onParsed?.(viewport);
    });
    return;
  }
  const writes = replayWriteChunks(frame.data);
  for (const [index, data] of writes.entries()) {
    terminal.write(data, index === writes.length - 1 ? () => {
      if (shouldRestore()) restoreViewport(viewport);
      onParsed?.(viewport);
    } : undefined);
  }
}
