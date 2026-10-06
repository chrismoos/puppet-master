import type { Terminal } from "@xterm/xterm";
import type { PtyFrame } from "@puppet-master/client-core/ws/pty";
import { writeTerminalFrame as writeFrame } from "@puppet-master/client-core/ws/terminalWriter";
import {
  captureTerminalViewport,
  restoreTerminalViewport,
  type TerminalViewportBookmark,
} from "./terminalViewport";

export { REPLAY_WRITE_CHUNK_BYTES } from "@puppet-master/client-core/ws/terminalWriter";

export function writeTerminalFrame(
  terminal: Terminal,
  frame: PtyFrame,
  viewport = captureTerminalViewport(terminal),
  onParsed?: (viewport: TerminalViewportBookmark) => void,
  shouldRestore: () => boolean = () => true,
): void {
  writeFrame(
    terminal,
    frame,
    viewport,
    (bookmark) => restoreTerminalViewport(terminal, bookmark),
    onParsed,
    shouldRestore,
  );
}
