import { afterEach, describe, expect, it, vi } from "vitest";
import { columnWidthAt, startColumnResize, trailingColumnWidthAt } from "./columnResize";

afterEach(() => vi.unstubAllGlobals());

/** A window and a body the drag can attach to, plus a way to fire the
 * pointer events it listens for. */
function stubDom() {
  const listeners = new Map<string, Set<(event: { clientX: number }) => void>>();
  const classes = new Set<string>();
  vi.stubGlobal("window", {
    addEventListener: (type: string, fn: (event: { clientX: number }) => void) => {
      const set = listeners.get(type) ?? new Set();
      set.add(fn);
      listeners.set(type, set);
    },
    removeEventListener: (type: string, fn: (event: { clientX: number }) => void) => {
      listeners.get(type)?.delete(fn);
    },
  });
  vi.stubGlobal("document", {
    body: { classList: { add: (c: string) => classes.add(c), remove: (c: string) => classes.delete(c) } },
  });
  return {
    classes,
    count: (type: string) => listeners.get(type)?.size ?? 0,
    fire: (type: string, clientX: number) => {
      for (const fn of [...(listeners.get(type) ?? [])]) fn({ clientX });
    },
  };
}

describe("columnWidthAt", () => {
  it("measures from the column's own left edge, not the viewport", () => {
    expect(columnWidthAt(500, 300, 100, 600)).toBe(200);
    expect(columnWidthAt(500, 0, 100, 600)).toBe(500);
  });

  it("holds the width inside the column's limits", () => {
    expect(columnWidthAt(120, 300, 100, 600)).toBe(100);
    expect(columnWidthAt(1200, 300, 100, 600)).toBe(600);
  });
});

describe("trailingColumnWidthAt", () => {
  it("measures back from the column's right edge", () => {
    expect(trailingColumnWidthAt(700, 1200, 360, 960)).toBe(500);
  });

  it("holds the width inside the column's limits", () => {
    expect(trailingColumnWidthAt(1100, 1200, 360, 960)).toBe(360);
    expect(trailingColumnWidthAt(0, 1200, 360, 960)).toBe(960);
  });
});

describe("startColumnResize", () => {
  it("widens a column anchored on the right as the pointer moves left", () => {
    const dom = stubDom();
    const widths: number[] = [];
    const commits: number[] = [];
    startColumnResize({
      originX: 1200,
      anchor: "right",
      min: 360,
      max: 960,
      start: 440,
      onWidth: (w) => widths.push(w),
      onCommit: (w) => commits.push(w),
    });

    dom.fire("pointermove", 700);
    dom.fire("pointermove", 100);
    expect(widths).toEqual([500, 960]);
    dom.fire("pointerup", 0);
    expect(commits).toEqual([960]);
  });

  it("reports each width as the pointer moves and commits once on release", () => {
    const dom = stubDom();
    const widths: number[] = [];
    const commits: number[] = [];
    const dragging: boolean[] = [];
    startColumnResize({
      originX: 300,
      min: 160,
      max: 620,
      start: 240,
      onWidth: (w) => widths.push(w),
      onCommit: (w) => commits.push(w),
      onDragging: (d) => dragging.push(d),
    });

    dom.fire("pointermove", 600);
    dom.fire("pointermove", 640.4);
    expect(widths).toEqual([300, 340.4]);
    expect(commits).toEqual([]);

    dom.fire("pointerup", 0);
    expect(commits).toEqual([340]);
    expect(dragging).toEqual([true, false]);
  });

  it("commits the width it started at when the pointer never moves", () => {
    const dom = stubDom();
    const commits: number[] = [];
    startColumnResize({
      originX: 300,
      min: 160,
      max: 620,
      start: 240,
      onWidth: () => undefined,
      onCommit: (w) => commits.push(w),
    });

    dom.fire("pointerup", 0);
    expect(commits).toEqual([240]);
  });

  it("stops listening and drops the drag cursor once released", () => {
    const dom = stubDom();
    startColumnResize({
      originX: 0,
      min: 160,
      max: 620,
      start: 240,
      onWidth: () => undefined,
      onCommit: () => undefined,
    });
    expect(dom.count("pointermove")).toBe(1);
    expect(dom.classes.has("resizing-col")).toBe(true);

    dom.fire("pointerup", 0);
    expect(dom.count("pointermove")).toBe(0);
    expect(dom.count("pointerup")).toBe(0);
    expect(dom.classes.has("resizing-col")).toBe(false);
  });
});
