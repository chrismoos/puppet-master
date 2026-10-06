export interface PtyFrame {
  data: Uint8Array;
  replay: boolean;
  /** The replay payload is a serialized state snapshot, not a ring tail. */
  snapshot?: boolean;
}

export interface TerminalSize {
  cols: number;
  rows: number;
}

export interface TerminalOwnership extends TerminalSize {
  local: boolean;
}

export type PtySink = (frame: PtyFrame) => void;

export function terminalSizeChanged(
  previous: TerminalSize | null,
  cols: number,
  rows: number,
): boolean {
  return previous === null || previous.cols !== cols || previous.rows !== rows;
}

/**
 * Holds PtyOutput frames that arrive between AttachPty and the terminal
 * becoming ready, then flushes them in order once a sink connects.
 */
export class PtyBuffer {
  private sink: PtySink | null = null;
  private held: PtyFrame[] = [];

  push(frame: PtyFrame): void {
    if (this.sink) {
      this.sink(frame);
    } else {
      this.held.push(frame);
    }
  }

  connect(sink: PtySink): void {
    this.sink = sink;
    const held = this.held;
    this.held = [];
    for (const frame of held) {
      sink(frame);
    }
  }

  disconnect(): void {
    this.sink = null;
  }

  clear(): void {
    this.held = [];
  }
}
