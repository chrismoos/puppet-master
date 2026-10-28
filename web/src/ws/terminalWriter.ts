import type { PtyFrame } from "@puppet-master/client-core/ws/pty";
import { REPLAY_WRITE_CHUNK_BYTES, SYNC_OUTPUT_START } from "@puppet-master/client-core/ws/terminalWriter";
import {
  captureTerminalViewport,
  restoreTerminalViewport,
  type TerminalViewportBookmark,
} from "./terminalViewport";

const SYNC_OUTPUT_END = new Uint8Array([0x1b, 0x5b, 0x3f, 0x32, 0x30, 0x32, 0x36, 0x6c]);

export { REPLAY_WRITE_CHUNK_BYTES };

interface TerminalFrameSurface {
  readonly buffer: { readonly active: { readonly type: string; readonly baseY: number; readonly viewportY: number } };
  readonly rows: number;
  write(data: string | Uint8Array, callback?: () => void): void;
  reset(): void;
  setReplaying(replaying: boolean): void;
  scrollToBottom(): void;
  scrollToLine(line: number): void;
}

export function writeTerminalFrame(
  terminal: TerminalFrameSurface,
  frame: PtyFrame,
  viewport = captureTerminalViewport(terminal),
  onParsed?: (viewport: TerminalViewportBookmark) => void,
  shouldRestore: () => boolean = () => true,
  prepareReplay?: () => void,
): void {
  const complete = (restore: boolean) => {
    if (restore && shouldRestore()) restoreTerminalViewport(terminal, viewport);
    onParsed?.(viewport);
  };
  if (!frame.replay) {
    terminal.write(frame.data, () => complete(viewport.linesFromBottom === 0));
    return;
  }
  terminal.reset();
  terminal.setReplaying(true);
  terminal.write(SYNC_OUTPUT_START);
  prepareReplay?.();
  const chunkCount = Math.max(1, Math.ceil(frame.data.byteLength / REPLAY_WRITE_CHUNK_BYTES));
  for (let index = 0; index < chunkCount; index += 1) {
    const start = index * REPLAY_WRITE_CHUNK_BYTES;
    const end = Math.min(frame.data.byteLength, start + REPLAY_WRITE_CHUNK_BYTES);
    terminal.write(frame.data.subarray(start, end));
  }
  terminal.write(SYNC_OUTPUT_END, () => {
    terminal.setReplaying(false);
    complete(true);
  });
}
