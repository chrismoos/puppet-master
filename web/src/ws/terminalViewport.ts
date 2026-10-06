import type { Terminal } from "@xterm/xterm";

export interface TerminalViewportBookmark {
  bufferType: string;
  linesFromBottom: number;
}

const STORAGE_PREFIX = "pm.terminalViewport.";

export function captureTerminalViewport(terminal: Terminal): TerminalViewportBookmark {
  const buffer = terminal.buffer.active;
  return {
    bufferType: buffer.type,
    linesFromBottom: Math.max(0, buffer.baseY - buffer.viewportY),
  };
}

/** Restore relative to the current buffer tail so cap trimming and snapshots
 * cannot turn an old absolute row into an unrelated content position. Calling
 * scrollToBottom for a zero-distance bookmark is intentional: besides pinning
 * the viewport after async writes, it clears xterm's internal user-scroll mode. */
export function restoreTerminalViewport(
  terminal: Terminal,
  bookmark: TerminalViewportBookmark,
): void {
  const buffer = terminal.buffer.active;
  if (buffer.type !== bookmark.bufferType) return;
  if (bookmark.linesFromBottom === 0) {
    terminal.scrollToBottom();
    return;
  }
  terminal.scrollToLine(Math.max(0, buffer.baseY - bookmark.linesFromBottom));
}

export function loadTerminalViewport(key: string): TerminalViewportBookmark | null {
  try {
    const stored = sessionStorage.getItem(`${STORAGE_PREFIX}${key}`);
    if (!stored) return null;
    const value = JSON.parse(stored) as Partial<TerminalViewportBookmark>;
    if (typeof value.bufferType !== "string"
      || typeof value.linesFromBottom !== "number"
      || !Number.isFinite(value.linesFromBottom)
      || value.linesFromBottom < 0) return null;
    return { bufferType: value.bufferType, linesFromBottom: value.linesFromBottom };
  } catch {
    return null;
  }
}

export function saveTerminalViewport(key: string, bookmark: TerminalViewportBookmark): void {
  try {
    sessionStorage.setItem(`${STORAGE_PREFIX}${key}`, JSON.stringify(bookmark));
  } catch {
    // Viewport persistence is best-effort when storage is unavailable.
  }
}
