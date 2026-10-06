/** Width a drag to `clientX` gives a column whose left edge sits at
 * `originX`, held inside the column's limits. */
export function columnWidthAt(
  clientX: number,
  originX: number,
  min: number,
  max: number,
): number {
  return Math.min(max, Math.max(min, clientX - originX));
}

/** Width a drag to `clientX` gives a column whose right edge sits at
 * `originX`, held inside the column's limits. */
export function trailingColumnWidthAt(
  clientX: number,
  originX: number,
  min: number,
  max: number,
): number {
  return Math.min(max, Math.max(min, originX - clientX));
}

export interface ColumnResize {
  /** Viewport x of the column's fixed edge: its left edge, or its right
   * edge when `anchor` is "right". */
  originX: number;
  /** Which edge stays put while the other is dragged. Left by default. */
  anchor?: "left" | "right";
  min: number;
  max: number;
  /** Width to commit if the pointer never moves. */
  start: number;
  onWidth: (width: number) => void;
  onCommit: (width: number) => void;
  onDragging?: (dragging: boolean) => void;
}

/**
 * Drags a column's trailing edge until the pointer is released, then
 * commits the width once. Pointer moves arrive on the window rather than
 * the handle so a fast drag that outruns the cursor keeps resizing.
 */
export function startColumnResize(resize: ColumnResize): void {
  const widthAt = resize.anchor === "right" ? trailingColumnWidthAt : columnWidthAt;
  let latest = Math.min(resize.max, Math.max(resize.min, resize.start));
  resize.onDragging?.(true);
  document.body.classList.add("resizing-col");
  const onMove = (event: PointerEvent) => {
    latest = widthAt(event.clientX, resize.originX, resize.min, resize.max);
    resize.onWidth(latest);
  };
  const onUp = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    document.body.classList.remove("resizing-col");
    resize.onDragging?.(false);
    resize.onCommit(Math.round(latest));
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
}
