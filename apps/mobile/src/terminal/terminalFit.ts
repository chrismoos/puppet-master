/**
 * Computes terminal column and row counts from a container's pixel
 * dimensions and xterm's cell metrics, reserving no scrollbar gutter.
 *
 * The mobile terminal hides xterm's native scrollbar and uses a custom
 * overlay indicator. The stock FitAddon always subtracts a 14 px gutter
 * for the overview ruler, which on phone-width screens rounds to one
 * extra column: the PTY gets cols that cannot be rendered, garbling
 * alternate-screen output. This function avoids that by using the full
 * container width, matching the web version's fitFullWidth().
 */
export function computeFitDimensions(
  hostWidth: number,
  hostHeight: number,
  paddingHorizontal: number,
  paddingVertical: number,
  cellWidth: number,
  cellHeight: number,
): { cols: number; rows: number } | null {
  if (cellWidth <= 0 || cellHeight <= 0) return null;
  const width = hostWidth - paddingHorizontal;
  const height = hostHeight - paddingVertical;
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) {
    return null;
  }
  return {
    cols: Math.max(2, Math.floor(width / cellWidth)),
    rows: Math.max(1, Math.floor(height / cellHeight)),
  };
}
