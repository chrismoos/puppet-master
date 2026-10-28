export const DEFAULT_TERMINAL_COLS = 80;

export interface TerminalGeometry {
  cols: number;
  rows: number;
}

export interface FittableTerminal {
  readonly cols: number;
  readonly rows: number;
  readonly cellSize: TerminalGeometry | null;
  fit(): boolean;
  resize(cols: number, rows: number): void;
}

export function measureFit(term: FittableTerminal): TerminalGeometry | null {
  term.fit();
  return term.cellSize;
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
export function fitFullWidth(term: FittableTerminal): boolean {
  return term.fit();
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
