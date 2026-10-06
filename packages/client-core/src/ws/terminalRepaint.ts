import type { TerminalSize } from "./pty";
import type { TerminalStreamStatus } from "./terminalSocket";

export interface RepaintSizeSource {
  readonly cols: number;
  readonly rows: number;
}

export interface RepaintHandle {
  resize(cols: number, rows: number): void;
  onStatus(listener: (status: TerminalStreamStatus) => void): () => void;
  /** The PTY's actual size as last echoed by the daemon, null before the
   * first echo arrives. */
  ptySize(): TerminalSize | null;
}

/** Coalesces rapid open and visibility edges into one PTY size assert. */
export const VIEWER_SIZE_ASSERT_DEBOUNCE_MS = 100;

export interface ViewerSizeOptions {
  debounceMs?: number;
  canAssert?: () => boolean;
}

/**
 * Decides when this viewer tells the PTY its size.
 *
 * One PTY has one size and an alternate-screen program lays out for it, so
 * two people genuinely watching at different sizes cannot both be correct.
 * This design chooses who wins rather than pretending otherwise: the viewer
 * that most recently opened, switched to, or regained visibility of the
 * terminal sets the size. A viewer that is not visible never asserts, not
 * even when its socket reconnects and replays. Asserts are edge-triggered on
 * those transitions, never level-triggered on state, so two visible viewers
 * cannot ping-pong. The debounce only coalesces rapid transitions.
 */
export interface ViewerSizeController {
  /** The viewer opened or switched to this terminal, which makes it visible. */
  opened(): void;
  /**
   * The window regained focus without ever having been hidden, so there is
   * no visibility edge to react to. Asserts only when the PTY no longer
   * holds this viewer's size: focus is not a transition in the policy above,
   * and asserting on it unconditionally would be the level-triggered
   * behavior that lets two focused windows trade the size back and forth.
   */
  refocused(): void;
  setVisible(visible: boolean): void;
  visible(): boolean;
  dispose(): void;
}

function sizeOf(term: RepaintSizeSource): TerminalSize | null {
  if (term.cols < 2 || term.rows < 1) return null;
  return { cols: term.cols, rows: term.rows };
}

/**
 * Wires the viewer size policy to one terminal stream. The viewer starts
 * hidden; callers mark the open moment with opened() and visibility edges
 * with setVisible().
 */
export function wireViewerSize(
  handle: RepaintHandle,
  term: RepaintSizeSource,
  options: ViewerSizeOptions = {},
): ViewerSizeController {
  const debounceMs = options.debounceMs ?? VIEWER_SIZE_ASSERT_DEBOUNCE_MS;
  let visible = false;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const cancel = () => {
    if (timer === null) return;
    clearTimeout(timer);
    timer = null;
  };

  const flush = () => {
    timer = null;
    if (!visible || options.canAssert?.() === false) return;
    const size = sizeOf(term);
    if (size) handle.resize(size.cols, size.rows);
  };

  const schedule = () => {
    cancel();
    timer = setTimeout(flush, debounceMs);
  };

  const unsubscribe = handle.onStatus((status) => {
    if (status.phase === "online" && visible) schedule();
  });

  return {
    opened() {
      visible = true;
      schedule();
    },
    refocused() {
      if (!visible || options.canAssert?.() === false) return;
      const size = sizeOf(term);
      if (!size) return;
      const pty = handle.ptySize();
      if (pty && pty.cols === size.cols && pty.rows === size.rows) return;
      schedule();
    },
    setVisible(next) {
      if (next === visible) return;
      visible = next;
      if (next) schedule();
      else cancel();
    },
    visible: () => visible,
    dispose() {
      cancel();
      unsubscribe();
    },
  };
}
