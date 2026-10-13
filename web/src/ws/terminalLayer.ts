import {
  applyEchoedPtySizeThenWrite,
  refreshSameSizeTerminal,
  type ResizableTerminal,
} from "@puppet-master/client-core/ws/terminalResize";
import { terminalSizeChanged, type TerminalSize } from "@puppet-master/client-core/ws/pty";

export function deliverLayerFrame(
  term: ResizableTerminal,
  ptySize: TerminalSize | null,
  write: () => void,
): boolean {
  return applyEchoedPtySizeThenWrite(term, ptySize, write);
}

export function isInitialSnapshotFrame(
  initialReplayPending: boolean,
  frame: { replay: boolean; snapshot?: boolean },
): boolean {
  return initialReplayPending && frame.replay && frame.snapshot === true;
}

export function showLayer(
  term: ResizableTerminal,
  fit: () => void,
  lastPty: TerminalSize | null,
  sendPtyResize: (cols: number, rows: number) => void,
): void {
  fit();
  if (terminalSizeChanged(lastPty, term.cols, term.rows)) {
    sendPtyResize(term.cols, term.rows);
  }
  refreshSameSizeTerminal(term);
}

export type MeasuredFitResult = "unchanged" | "resized" | "height-only" | "reset";

export interface FitTerminal extends ResizableTerminal {
  reset?(): void;
}

export function applyMeasuredFit(
  term: FitTerminal,
  measured: { cols: number; rows: number } | null,
  painted: boolean,
): MeasuredFitResult {
  if (!measured) return "unchanged";
  if (measured.cols === term.cols && measured.rows === term.rows) return "unchanged";
  if (!painted) {
    term.resize(measured.cols, measured.rows);
    return "resized";
  }
  if (measured.cols === term.cols) {
    term.resize(term.cols, measured.rows);
    return "height-only";
  }
  if (typeof term.reset !== "function") return "unchanged";
  term.reset();
  term.resize(measured.cols, measured.rows);
  return "reset";
}

export interface RenderTerminal {
  rows: number;
  refresh(start: number, end: number): void;
  onRender(listener: () => void): { dispose(): void };
}

export function waitForTerminalRender(
  term: RenderTerminal,
  ready: () => boolean,
  rendered: () => void,
): { dispose(): void } {
  let disposed = false;
  const listener = term.onRender(() => {
    if (disposed || !ready()) return;
    disposed = true;
    listener.dispose();
    rendered();
  });
  term.refresh(0, term.rows - 1);
  return {
    dispose() {
      if (disposed) return;
      disposed = true;
      listener.dispose();
    },
  };
}

/**
 * Whether a layer is on screen while the layer being shown waits for a
 * snapshot.
 *
 * A layer that has painted before keeps the previous screen up and swaps
 * in one step, so a size change never shows a half-drawn pane. A layer
 * with nothing to show takes the pane immediately instead, because the
 * alternative is leaving another session's output under this session's
 * header.
 */
export function pendingShowVisibility(
  incoming: { key: string; initialReplayPending: boolean },
  candidate: { key: string; visible: boolean },
  previousKey: string | null,
): boolean {
  if (candidate.key === incoming.key) return incoming.initialReplayPending;
  if (incoming.initialReplayPending) return false;
  return candidate.key === previousKey && candidate.visible;
}

/** Keystrokes offered to a layer whose socket is not open yet. */
export interface HeldInput {
  data: Uint8Array;
  submitted: boolean;
}

/**
 * Holds a keystroke for a layer that has no socket yet.
 *
 * A pane takes the screen as soon as it is asked for, which is before its
 * terminal is attached, and what the reader types in that window has to
 * reach the session rather than vanish. Past the cap the oldest held
 * keystrokes go: a reader who kept typing into an unreachable terminal
 * wants what they typed last.
 */
export function holdInput(held: HeldInput[], input: HeldInput, capBytes: number): HeldInput[] {
  const next = [...held, input];
  let bytes = next.reduce((total, entry) => total + entry.data.byteLength, 0);
  while (next.length > 1 && bytes > capBytes) {
    bytes -= next[0].data.byteLength;
    next.shift();
  }
  return next;
}

/**
 * Whether a first snapshot left history behind, from the lines it painted.
 *
 * The controller sends every line it has up to the bound, so a buffer
 * shorter than the bound holds the whole history and asking for the rest
 * would only rewrite the buffer for nothing.
 */
export function historyWasTruncated(bufferLines: number, bound: number): boolean {
  return bufferLines >= bound;
}
