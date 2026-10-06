import type { TerminalSize } from "./pty";

export interface ResizableTerminal {
  readonly cols: number;
  readonly rows: number;
  resize(cols: number, rows: number): void;
  refresh?(start: number, end: number): void;
}

export function applyEchoedPtySize(
  term: ResizableTerminal,
  size: TerminalSize | null | undefined,
  options?: { freeze?: boolean },
): boolean {
  if (options?.freeze) return false;
  if (!size || size.cols < 2 || size.rows < 1) return false;
  if (term.cols === size.cols && term.rows === size.rows) return false;
  term.resize(size.cols, size.rows);
  return true;
}

/**
 * Write only when the emulator already matches the PTY echo. Never resize
 * here. xterm reflows on width: a line exactly as wide as the terminal wraps
 * when it narrows, and growing back does not rejoin it — the tail stays on
 * its own row and everything below it sits one line lower for good. Painting
 * at 80 cols then resizing to 120 is that path. Hidden layers may
 * applyEchoedPtySize before any cells are parsed so live bytes track the
 * echo; after a paint, width stays frozen until a reset or a live full
 * redraw.
 */
export function applyEchoedPtySizeThenWrite(
  term: ResizableTerminal,
  size: TerminalSize | null | undefined,
  write: () => void,
): boolean {
  if (!size || size.cols < 2 || size.rows < 1) {
    write();
    return true;
  }
  if (term.cols === size.cols && term.rows === size.rows) {
    write();
    return true;
  }
  return false;
}

export function refreshSameSizeTerminal(term: ResizableTerminal): void {
  if (term.rows > 0) term.refresh?.(0, term.rows - 1);
}
