import type { Terminal } from "@xterm/xterm";

export const XTERM_DEFAULT_COLS = 80;

export interface TerminalGeometry {
  cols: number;
  rows: number;
}

interface FitInternals {
  _core?: {
    _renderService?: {
      clear?: () => void;
      dimensions?: { css?: { cell?: { width?: number; height?: number } } };
    };
  };
}

export function measureFit(term: Terminal): TerminalGeometry | null {
  const element = term.element;
  const host = element?.parentElement;
  if (!element || !host) return null;
  const core = (term as unknown as FitInternals)._core;
  const cell = core?._renderService?.dimensions?.css?.cell;
  const cellWidth = cell?.width ?? 0;
  const cellHeight = cell?.height ?? 0;
  if (cellWidth <= 0 || cellHeight <= 0) return null;

  const hostStyle = window.getComputedStyle(host);
  const elementStyle = window.getComputedStyle(element);
  const paddingHorizontal =
    parseFloat(elementStyle.getPropertyValue("padding-left")) +
    parseFloat(elementStyle.getPropertyValue("padding-right"));
  const paddingVertical =
    parseFloat(elementStyle.getPropertyValue("padding-top")) +
    parseFloat(elementStyle.getPropertyValue("padding-bottom"));
  const width = parseInt(hostStyle.getPropertyValue("width"), 10) - paddingHorizontal;
  const height = parseInt(hostStyle.getPropertyValue("height"), 10) - paddingVertical;
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) {
    return null;
  }

  return {
    cols: Math.max(2, Math.floor(width / cellWidth)),
    rows: Math.max(1, Math.floor(height / cellHeight)),
  };
}

export function isRealPaneFit(hostWidth: number, measuredCols: number | null): boolean {
  if (hostWidth <= 0 || measuredCols === null) return false;
  return true;
}

/**
 * Sizes the terminal to the full host width and height. Unlike the fit
 * addon this reserves no scrollbar gutter: the transient scrollbar
 * overlays the content at the right edge instead of narrowing every
 * terminal by a column strip.
 */
export function fitFullWidth(term: Terminal): boolean {
  const measured = measureFit(term);
  if (!measured) return false;
  if (measured.cols === term.cols && measured.rows === term.rows) return false;
  const core = (term as unknown as FitInternals)._core;
  core?._renderService?.clear?.();
  term.resize(measured.cols, measured.rows);
  return true;
}

/**
 * Chooses the geometry a spawn should ask for. A mounted pane already knows
 * its own size, so `probe` runs only when none does and the cost of building
 * a throwaway terminal is unavoidable.
 */
export function spawnGeometry(
  panes: Iterable<{ cols: number; rows: number }>,
  probe: () => TerminalGeometry | null,
): TerminalGeometry | null {
  for (const pane of panes) {
    if (pane.cols > 0 && pane.rows > 0) return { cols: pane.cols, rows: pane.rows };
  }
  return probe();
}
