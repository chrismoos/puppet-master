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
