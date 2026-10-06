// Platform-neutral layout decisions that keep the terminal's bottom row
// visible when the software keyboard is up. The native shell shrinks its
// layout around the WebView, and the page independently sizes the terminal
// container to the visual viewport, so the bottom row stays above the
// keyboard even when only one of the two signals arrives.

export interface ViewportSample {
  /** Layout viewport height (window.innerHeight). */
  layoutHeightPx: number;
  /** Visual viewport height, or null where the API is unavailable. */
  visualHeightPx: number | null;
}

/**
 * Height the terminal container should occupy. The visual viewport shrinks
 * when the keyboard overlays the page without resizing the layout viewport,
 * so the smaller of the two is the space actually visible.
 */
export function terminalHeightPx(sample: ViewportSample): number {
  const layout = Math.max(0, Math.floor(sample.layoutHeightPx));
  const visual = sample.visualHeightPx;
  if (visual === null || !Number.isFinite(visual) || visual <= 0) return layout;
  return Math.min(layout, Math.floor(visual));
}

/** True when the viewport is following the bottom of the scrollback. */
export function followingBottom(viewportY: number, baseY: number): boolean {
  return viewportY >= baseY;
}

/**
 * After a refit that may have changed terminal dimensions, compute the
 * scrollLines delta needed to restore the pre-refit viewport position.
 * Returns 0 when no correction is needed.
 *
 * `savedViewportY` is the viewportY captured before the refit.
 * `currentViewportY` is the viewportY xterm settled on after the refit.
 * `newBaseY` is the post-refit baseY (the maximum valid viewportY).
 */
export function viewportRestoreDelta(
  savedViewportY: number,
  currentViewportY: number,
  newBaseY: number,
): number {
  const target = Math.min(savedViewportY, newBaseY);
  return target - currentViewportY;
}

/** A replay rebuilds xterm from a reset sequence. Preserve the reader's
 * distance from the bottom across that rebuild instead of preserving an
 * absolute row number from the old buffer. */
export function linesFromBottom(viewportY: number, baseY: number): number {
  return Math.max(0, baseY - viewportY);
}

/** Resolve a bottom-relative replay bookmark against the rebuilt buffer. */
export function viewportForLinesFromBottom(baseY: number, bookmark: number): number {
  return Math.max(0, baseY - Math.max(0, bookmark));
}
