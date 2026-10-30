import { GHOSTTY_CURSOR_STYLE } from "./abi";
import {
  GHOSTTY_CELL_WIDE,
  ghosttyColorsEqual,
  type GhosttyCell,
  type GhosttyColor,
  type GhosttySnapshot,
} from "./core";

export interface GhosttyCellMetrics {
  readonly width: number;
  readonly height: number;
  readonly baseline: number;
}

export interface GhosttyCellRange {
  readonly start: { readonly x: number; readonly y: number };
  readonly end: { readonly x: number; readonly y: number };
}

const DEFAULT_SELECTION_BACKGROUND = "rgba(72, 122, 191, 0.35)";
const POWERLINE_SOLID_RIGHT = "\ue0b0";
const POWERLINE_SOLID_LEFT = "\ue0b2";

const BLOCK_FIRST = 0x2580;
const BLOCK_FULL = 0x2588;
const BLOCK_LEFT_EIGHTH = 0x258f;
const BLOCK_RIGHT_HALF = 0x2590;
const BLOCK_UPPER_EIGHTH = 0x2594;
const BLOCK_RIGHT_EIGHTH = 0x2595;
const BLOCK_QUADRANT_FIRST = 0x2596;
const BLOCK_LAST = 0x259f;
const SHADE_FIRST = 0x2591;
const SHADE_LAST = 0x2593;
const SHADE_LEVELS = 4;
const BLOCK_EIGHTHS = 8;
const QUADRANTS = [
  [0, 1], [1, 1], [0, 0],
  [0, 0, 0, 1, 1, 1], [0, 0, 1, 1],
  [0, 0, 1, 0, 0, 1], [0, 0, 1, 0, 1, 1],
  [1, 0], [1, 0, 0, 1], [1, 0, 0, 1, 1, 1],
];

function isBlockElement(text: string): boolean {
  const code = text.codePointAt(0) ?? 0;
  return text.length === 1 && code >= BLOCK_FIRST && code <= BLOCK_LAST;
}

function fillBlockElement(
  context: CanvasRenderingContext2D,
  text: string,
  left: number,
  top: number,
  metrics: GhosttyCellMetrics,
): void {
  const code = text.charCodeAt(0);
  const transform = context.getTransform();
  const snapX = (x: number) => (Math.round(x * transform.a + transform.e) - transform.e) / transform.a;
  const snapY = (y: number) => (Math.round(y * transform.d + transform.f) - transform.f) / transform.d;
  const rectangle = (x: number, y: number, width: number, height: number) => {
    const x1 = snapX(left + x * metrics.width);
    const y1 = snapY(top + y * metrics.height);
    const x2 = snapX(left + (x + width) * metrics.width);
    const y2 = snapY(top + (y + height) * metrics.height);
    context.fillRect(x1, y1, x2 - x1, y2 - y1);
  };
  if (code >= SHADE_FIRST && code <= SHADE_LAST) {
    context.save();
    context.globalAlpha *= (code - SHADE_FIRST + 1) / SHADE_LEVELS;
    rectangle(0, 0, 1, 1);
    context.restore();
  } else if (code === BLOCK_FIRST) {
    rectangle(0, 0, 1, 1 / 2);
  } else if (code <= BLOCK_FULL) {
    const height = (code - BLOCK_FIRST) / BLOCK_EIGHTHS;
    rectangle(0, 1 - height, 1, height);
  } else if (code <= BLOCK_LEFT_EIGHTH) {
    rectangle(0, 0, (BLOCK_RIGHT_HALF - code) / BLOCK_EIGHTHS, 1);
  } else if (code === BLOCK_RIGHT_HALF) {
    rectangle(1 / 2, 0, 1 / 2, 1);
  } else if (code === BLOCK_UPPER_EIGHTH) {
    rectangle(0, 0, 1, 1 / BLOCK_EIGHTHS);
  } else if (code === BLOCK_RIGHT_EIGHTH) {
    rectangle(1 - 1 / BLOCK_EIGHTHS, 0, 1 / BLOCK_EIGHTHS, 1);
  } else {
    const quadrants = QUADRANTS[code - BLOCK_QUADRANT_FIRST]!;
    for (let index = 0; index < quadrants.length; index += 2) {
      rectangle(quadrants[index]! / 2, quadrants[index + 1]! / 2, 1 / 2, 1 / 2);
    }
  }
}

function cssColor(color: GhosttyColor): string {
  return `rgb(${color.r}, ${color.g}, ${color.b})`;
}

function sameTextStyle(left: GhosttyCell, right: GhosttyCell): boolean {
  // Selection only tints the background; splitting text runs shifts glyph spacing when advances differ from cell width.
  return (
    ghosttyColorsEqual(left.foreground, right.foreground) &&
    left.bold === right.bold &&
    left.italic === right.italic &&
    left.invisible === right.invisible
  );
}

function isSolidPowerlineSeparator(text: string): boolean {
  return text === POWERLINE_SOLID_RIGHT || text === POWERLINE_SOLID_LEFT;
}

// Fallback fonts inset or scale these private-use glyphs, leaving a gap where
// the separator should meet the next cell. Fill the triangle from the cell's
// own edges instead, as native terminals do.
function fillSolidPowerlineSeparator(
  context: CanvasRenderingContext2D,
  text: string,
  left: number,
  top: number,
  metrics: GhosttyCellMetrics,
): void {
  const right = left + metrics.width;
  const [base, point] = text === POWERLINE_SOLID_RIGHT ? [left, right] : [right, left];
  context.beginPath();
  context.moveTo(base, top);
  context.lineTo(point, top + metrics.height / 2);
  context.lineTo(base, top + metrics.height);
  context.closePath();
  context.fill();
}

function ghosttyTextRunEnd(
  cells: readonly GhosttyCell[],
  start: number,
  sameStyle: (cell: GhosttyCell) => boolean,
): number {
  let end = start + 1;
  while (end < cells.length) {
    const next = cells[end];
    if (!next) break;
    if (next.wide === GHOSTTY_CELL_WIDE.spacerTail) {
      end += 1;
      continue;
    }
    if (
      next.text.length === 0 ||
      isSolidPowerlineSeparator(next.text) ||
      isBlockElement(next.text) ||
      !sameStyle(next)
    ) break;
    end += 1;
  }
  return end;
}

function fontForCell(cell: GhosttyCell, fontSize: number, fontFamily: string): string {
  const style = cell.italic ? "italic" : "normal";
  const weight = cell.bold ? "700" : "400";
  return `${style} ${weight} ${fontSize}px ${fontFamily}`;
}

export function measureGhosttyCell(
  context: CanvasRenderingContext2D,
  fontSize: number,
  fontFamily: string,
): GhosttyCellMetrics {
  context.font = `normal 400 ${fontSize}px ${fontFamily}`;
  const widthMeasurement = context.measureText("M");
  const verticalMeasurement = context.measureText("Mg");
  const ascent = verticalMeasurement.actualBoundingBoxAscent || fontSize;
  const descent = verticalMeasurement.actualBoundingBoxDescent;
  const glyphHeight = ascent + descent;
  const height = Math.max(1, Math.round(fontSize * 1.35), Math.ceil(glyphHeight));
  return {
    width: Math.max(1, widthMeasurement.width),
    height,
    baseline: Math.round((height - glyphHeight) / 2 + ascent),
  };
}

export function terminalGridSize(
  width: number,
  height: number,
  metrics: GhosttyCellMetrics,
  padding: number,
): { cols: number; rows: number } {
  return {
    cols: Math.max(1, Math.floor((width - padding * 2) / metrics.width)),
    rows: Math.max(1, Math.floor((height - padding * 2) / metrics.height)),
  };
}

export function renderGhosttySnapshot(options: {
  readonly context: CanvasRenderingContext2D;
  readonly snapshot: GhosttySnapshot;
  readonly metrics: GhosttyCellMetrics;
  readonly fontSize: number;
  readonly fontFamily: string;
  readonly padding: number;
  readonly forceFull: boolean;
  readonly cursorOn: boolean;
  readonly previousCursorY?: number | null;
  readonly focused?: boolean;
  readonly selectionBackground?: string;
  readonly hoveredLinkRange?: GhosttyCellRange | null;
  readonly originY?: number;
}): void {
  const {
    context,
    snapshot,
    metrics,
    fontSize,
    fontFamily,
    padding,
    forceFull,
    cursorOn,
    previousCursorY,
  } = options;
  const focused = options.focused ?? true;
  const selectionBackground = options.selectionBackground ?? DEFAULT_SELECTION_BACKGROUND;
  const hoveredLinkRange = options.hoveredLinkRange ?? null;
  const originY = options.originY ?? padding;
  const rowsToDraw = forceFull
    ? Array.from({ length: snapshot.rows }, (_, index) => index)
    : [...snapshot.dirtyRows];
  if (
    previousCursorY !== null &&
    previousCursorY !== undefined &&
    previousCursorY >= 0 &&
    !rowsToDraw.includes(previousCursorY)
  ) {
    rowsToDraw.push(previousCursorY);
  }
  if (snapshot.cursorVisible && snapshot.cursorY >= 0 && !rowsToDraw.includes(snapshot.cursorY)) {
    rowsToDraw.push(snapshot.cursorY);
  }

  if (forceFull) {
    context.save();
    context.resetTransform();
    context.fillStyle = cssColor(snapshot.background);
    context.fillRect(0, 0, context.canvas.width, context.canvas.height);
    context.restore();
  }

  context.textBaseline = "alphabetic";
  for (const rowIndex of rowsToDraw) {
    const row = snapshot.rowData[rowIndex];
    if (!row) continue;
    const top = originY + rowIndex * metrics.height;

    context.fillStyle = cssColor(snapshot.background);
    context.fillRect(padding, top, snapshot.cols * metrics.width, metrics.height);

    let backgroundStart = 0;
    while (backgroundStart < row.cells.length) {
      const first = row.cells[backgroundStart];
      if (!first) break;
      let backgroundEnd = backgroundStart + 1;
      while (backgroundEnd < row.cells.length) {
        const next = row.cells[backgroundEnd];
        if (
          !next ||
          next.selected !== first.selected ||
          !ghosttyColorsEqual(next.background, first.background)
        ) {
          break;
        }
        backgroundEnd += 1;
      }
      if (first.selected || !ghosttyColorsEqual(first.background, snapshot.background)) {
        const left = padding + backgroundStart * metrics.width;
        const width = (backgroundEnd - backgroundStart) * metrics.width;
        if (!ghosttyColorsEqual(first.background, snapshot.background)) {
          context.fillStyle = cssColor(first.background);
          context.fillRect(left, top, width, metrics.height);
        }
        if (first.selected) {
          context.fillStyle = selectionBackground;
          context.fillRect(left, top, width, metrics.height);
        }
      }
      backgroundStart = backgroundEnd;
    }

    let runStart = 0;
    while (runStart < row.cells.length) {
      const first = row.cells[runStart];
      if (!first) break;
      if (first.text.length === 0) {
        runStart += 1;
        continue;
      }
      if (isBlockElement(first.text)) {
        if (!first.invisible) {
          context.fillStyle = cssColor(first.foreground);
          fillBlockElement(context, first.text, padding + runStart * metrics.width, top, metrics);
        }
        runStart += 1;
        continue;
      }
      if (isSolidPowerlineSeparator(first.text)) {
        if (!first.invisible) {
          context.fillStyle = cssColor(first.foreground);
          fillSolidPowerlineSeparator(
            context,
            first.text,
            padding + runStart * metrics.width,
            top,
            metrics,
          );
        }
        runStart += 1;
        continue;
      }
      const runEnd = ghosttyTextRunEnd(row.cells, runStart, (cell) => sameTextStyle(cell, first));
      const text = row.cells
        .slice(runStart, runEnd)
        .map((cell) => cell.text)
        .join("");
      if (!first.invisible && text.trim().length > 0) {
        context.save();
        context.beginPath();
        context.rect(
          padding + runStart * metrics.width,
          top,
          (runEnd - runStart) * metrics.width,
          metrics.height,
        );
        context.clip();
        context.font = fontForCell(first, fontSize, fontFamily);
        context.fillStyle = cssColor(first.foreground);
        context.fillText(
          text,
          padding + runStart * metrics.width,
          top + metrics.baseline,
          (runEnd - runStart) * metrics.width,
        );
        context.restore();
      }
      runStart = runEnd;
    }

    for (let column = 0; column < row.cells.length; column += 1) {
      const cell = row.cells[column];
      const hoveredLink =
        hoveredLinkRange !== null &&
        rowIndex >= hoveredLinkRange.start.y &&
        rowIndex <= hoveredLinkRange.end.y &&
        (rowIndex > hoveredLinkRange.start.y || column >= hoveredLinkRange.start.x) &&
        (rowIndex < hoveredLinkRange.end.y || column <= hoveredLinkRange.end.x);
      if (!cell || (!cell.underline && !cell.strikethrough && !cell.overline && !hoveredLink)) {
        continue;
      }
      context.fillStyle = cssColor(cell.foreground);
      const left = padding + column * metrics.width;
      if (cell.underline || hoveredLink) {
        context.fillRect(left, top + metrics.height - 2, metrics.width, 1);
      }
      if (cell.strikethrough) {
        context.fillRect(left, top + Math.floor(metrics.height * 0.55), metrics.width, 1);
      }
      if (cell.overline) context.fillRect(left, top + 1, metrics.width, 1);
    }
  }

  if (cursorOn && snapshot.cursorVisible && snapshot.cursorX >= 0 && snapshot.cursorY >= 0) {
    const left = padding + snapshot.cursorX * metrics.width;
    const top = originY + snapshot.cursorY * metrics.height;
    context.fillStyle = cssColor(snapshot.cursor);
    if (!focused) {
      context.strokeStyle = cssColor(snapshot.cursor);
      context.strokeRect(left + 0.5, top + 0.5, metrics.width - 1, metrics.height - 1);
    } else if (snapshot.cursorStyle === GHOSTTY_CURSOR_STYLE.bar) {
      context.fillRect(left, top, 2, metrics.height);
    } else if (snapshot.cursorStyle === GHOSTTY_CURSOR_STYLE.underline) {
      context.fillRect(left, top + metrics.height - 2, metrics.width, 2);
    } else if (snapshot.cursorStyle === GHOSTTY_CURSOR_STYLE.hollowBlock) {
      context.strokeStyle = cssColor(snapshot.cursor);
      context.strokeRect(left + 0.5, top + 0.5, metrics.width - 1, metrics.height - 1);
    } else {
      context.fillRect(left, top, metrics.width, metrics.height);
      const cell = snapshot.rowData[snapshot.cursorY]?.cells[snapshot.cursorX];
      if (cell?.text) {
        context.fillStyle = cssColor(snapshot.background);
        if (isBlockElement(cell.text)) {
          fillBlockElement(context, cell.text, left, top, metrics);
        } else if (isSolidPowerlineSeparator(cell.text)) {
          fillSolidPowerlineSeparator(context, cell.text, left, top, metrics);
        } else {
          context.font = fontForCell(cell, fontSize, fontFamily);
          context.fillText(cell.text, left, top + metrics.baseline, metrics.width);
        }
      }
    }
  }
}
