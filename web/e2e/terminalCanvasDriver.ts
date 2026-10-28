import type { Locator } from "@playwright/test";

export async function terminalCanvasGeometry(terminal: Locator) {
  return terminal.evaluate((element) => {
    const canvas = element.querySelector<HTMLCanvasElement>(".ghostty-canvas");
    if (!canvas) throw new Error("terminal canvas is not mounted");
    const bounds = canvas.getBoundingClientRect();
    const style = getComputedStyle(element);
    const cellWidth = Number.parseFloat(style.getPropertyValue("--ghostty-cell-width"));
    const cellHeight = Number.parseFloat(style.getPropertyValue("--ghostty-cell-height"));
    const padding = Number.parseFloat(style.getPropertyValue("--ghostty-content-padding"));
    const originY = Number.parseFloat(style.getPropertyValue("--ghostty-content-origin-y"));
    if (![cellWidth, cellHeight, padding, originY].every(Number.isFinite) || cellWidth <= 0 || cellHeight <= 0) {
      throw new Error("terminal canvas metrics are not ready");
    }
    return {
      left: bounds.left + padding,
      top: bounds.top + originY,
      width: bounds.width,
      height: bounds.height,
      cellWidth,
      cellHeight,
      padding,
    };
  });
}

export async function watchTerminalPointer(terminal: Locator) {
  await terminal.evaluate((element) => {
    const diagnostic = element as HTMLElement & { __canvasDriverEvents?: Array<Record<string, unknown>>; __pmGhosttyDiagnostic?: () => unknown };
    diagnostic.__canvasDriverEvents = [];
    const snapshot = () => diagnostic.__pmGhosttyDiagnostic?.() ?? null;
    for (const type of ["pointermove", "pointerdown", "pointerup"]) {
      element.addEventListener(type, (event) => {
        const pointer = event as PointerEvent;
        const target = event.target as Element;
        const entry: Record<string, unknown> = { type, x: pointer.clientX, y: pointer.clientY, target: target.className, before: snapshot() };
        diagnostic.__canvasDriverEvents!.push(entry);
        if (diagnostic.__canvasDriverEvents!.length > 24) {
          diagnostic.__canvasDriverEvents = [...diagnostic.__canvasDriverEvents!.slice(0, 8), ...diagnostic.__canvasDriverEvents!.slice(-16)];
        }
        queueMicrotask(() => {
          entry.after = snapshot();
          requestAnimationFrame(() => { entry.frame = snapshot(); });
        });
      }, { capture: true });
    }
  });
}

export async function terminalCanvasDiagnostics(terminal: Locator, point?: { x: number; y: number }) {
  return terminal.evaluate((element, position) => {
    const bounds = (node: Element | null) => node?.getBoundingClientRect().toJSON() ?? null;
    const style = getComputedStyle(element);
    const input = element.querySelector(".ghostty-input");
    const hit = position ? document.elementFromPoint(position.x, position.y) : null;
    const stage = (window as unknown as { __pmStage?: {
      layers: Map<string, { el: HTMLElement; term: {
        renderSnapshot(): { cursorX: number; cursorY: number; cursorVisible: boolean; rowData: Array<{ text: string }> } | null;
      } }>;
    } }).__pmStage;
    const layer = [...stage?.layers.values() ?? []].find((candidate) => candidate.el === element.closest(".term-layer"));
    const snapshot = layer?.term.renderSnapshot();
    return {
      point: position,
      runtime: (element as HTMLElement & { __pmGhosttyDiagnostic?: () => unknown }).__pmGhosttyDiagnostic?.() ?? null,
      root: bounds(element),
      canvas: bounds(element.querySelector(".ghostty-canvas")),
      input: bounds(input),
      inputStyle: input?.getAttribute("style"),
      metrics: Object.fromEntries(["--ghostty-cell-width", "--ghostty-cell-height", "--ghostty-content-padding", "--ghostty-content-origin-y"].map((key) => [key, style.getPropertyValue(key)])),
      elementAtPoint: hit ? { tag: hit.tagName, className: hit.className, bounds: bounds(hit) } : null,
      hover: [...document.querySelectorAll(".ghostty-link-hover")].map((node) => ({ className: node.className, bounds: bounds(node) })),
      hints: [...document.querySelectorAll(".terminal-pm-link-hint")].map((node) => ({ text: node.textContent, parent: node.parentElement?.className })),
      snapshot: snapshot ? { cursorX: snapshot.cursorX, cursorY: snapshot.cursorY, cursorVisible: snapshot.cursorVisible, rows: snapshot.rowData.slice(0, 8).map((row) => row.text) } : null,
      events: (element as HTMLElement & { __canvasDriverEvents?: unknown[] }).__canvasDriverEvents,
    };
  }, point);
}
