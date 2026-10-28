import { TerminalStage } from "../src/ws/terminal";
import { TerminalThemeController } from "../src/theme/controller";
import { preloadTerminalFonts } from "../src/fonts";
import type { TerminalLayerDebug } from "../src/ws/terminalDiagnostics";
import { createTerminalProbeClient } from "./terminalProbeClient";
import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
import "../src/styles.css";

const frame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
const encoder = new TextEncoder();
const fixture = createTerminalProbeClient();
const client = fixture.client;
const inputs = fixture.inputs;
const resizes = fixture.resizes;
const DIAGNOSTIC_SCREEN_ROWS = 2;
let initialGeometry: { cols: number; rows: number } | null = null;

const host = document.querySelector<HTMLElement>("#host")!;
Object.assign(document.body.style, { margin: "0", width: "100vw", height: "100vh", overflow: "hidden" });
Object.assign(host.style, { position: "relative", width: "0", height: "0" });

class InertResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}
window.ResizeObserver = InertResizeObserver as unknown as typeof ResizeObserver;

const stage = new TerminalStage(client, new TerminalThemeController());

function layer(key: string): TerminalLayerDebug {
  const result = stage.debugSnapshot().find((entry) => entry.key === key);
  if (!result) throw new Error(`missing terminal layer ${key}`);
  return result;
}

function visibleCanvas(): HTMLCanvasElement {
  const canvas = host.querySelector<HTMLCanvasElement>(
    '.term-layer[style*="visibility: visible"] .ghostty-canvas',
  );
  if (!canvas) throw new Error("missing visible terminal canvas");
  return canvas;
}

function layerTimeoutDetails(key: string, attempts: number) {
  const snapshots = stage.debugSnapshot().map(({ screenText, ...entry }) => ({
    ...entry,
    screenText: screenText.slice(0, DIAGNOSTIC_SCREEN_ROWS),
  }));
  const terminal = host.querySelector<HTMLElement>(
    '.term-layer[style*="visibility: visible"] .ghostty-terminal',
  );
  const canvas = terminal?.querySelector<HTMLCanvasElement>(".ghostty-canvas");
  const rectangle = (element: Element | null | undefined) => {
    if (!element) return null;
    const bounds = element.getBoundingClientRect();
    return { x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height };
  };
  return {
    key,
    attempts,
    initialGeometry,
    stage: snapshots,
    host: {
      width: host.clientWidth,
      height: host.clientHeight,
      bounds: rectangle(host),
    },
    terminal: terminal ? {
      bounds: rectangle(terminal),
      grid: {
        cols: getComputedStyle(terminal).getPropertyValue("--ghostty-grid-cols"),
        rows: getComputedStyle(terminal).getPropertyValue("--ghostty-grid-rows"),
      },
    } : null,
    canvas: canvas ? {
      width: canvas.width,
      height: canvas.height,
      bounds: rectangle(canvas),
    } : null,
    opens: Object.fromEntries(fixture.opens),
    closes: Object.fromEntries(fixture.closes),
    resizes: Object.fromEntries(resizes),
    replay: snapshots.find((entry) => entry.key === key)?.socket ?? null,
  };
}

async function waitForLayer(key: string, predicate: (debug: TerminalLayerDebug) => boolean, label: string): Promise<TerminalLayerDebug> {
  const attemptLimit = 1_200;
  for (let attempt = 0; attempt < attemptLimit; attempt += 1) {
    const current = stage.debugSnapshot().find((entry) => entry.key === key);
    if (current && predicate(current)) return current;
    await frame();
  }
  throw new Error(`timed out waiting for ${label}: ${JSON.stringify(layerTimeoutDetails(key, attemptLimit))}`);
}

function snapshotLayer(key: string) {
  const debug = layer(key);
  return {
    cols: debug.cols,
    rows: debug.rows,
    mode: debug.mouseProtocol,
    buffer: debug.bufferType,
    baseY: debug.baseY,
    viewportY: debug.viewportY,
    screenText: debug.screenText,
  };
}

async function dispatchWheel(deltaY: number): Promise<void> {
  visibleCanvas().dispatchEvent(new WheelEvent("wheel", { deltaY, bubbles: true, cancelable: true }));
  await frame();
}

await preloadTerminalFonts();
stage.mount(host);
stage.show(1n);
await waitForLayer("s:1", (debug) => debug.charSizeValid === true, "the initial Canvas grid");
const initialGrid = { cols: layer("s:1").cols, rows: layer("s:1").rows };
initialGeometry = initialGrid;
Object.assign(host.style, { width: "100%", height: "100%" });
stage.mount(host);
await frame();
await frame();
const lifecycle = await waitForLayer(
  "s:1",
  (debug) => debug.cols > initialGrid.cols && debug.rows > initialGrid.rows,
  "the terminal to recover after its host layout settles",
);
const lifecycleRecovery = {
  cols: lifecycle.cols,
  rows: lifecycle.rows,
  canvas: (() => {
    const rect = visibleCanvas().getBoundingClientRect();
    return [rect.width, rect.height];
  })(),
};

fixture.write("s:1", encoder.encode("claude normal\r\n".repeat(500) + "\u001b[?1049h\u001b[?1002h\u001b[?1006hCLAUDE ALT"), true);
await waitForLayer("s:1", (debug) => debug.bufferType === "alternate" && debug.mouseProtocol !== "none", "Claude alternate-screen mouse mode");
const claudeState = snapshotLayer("s:1");

stage.show(2n);
fixture.write("s:2", encoder.encode("codex native scrollback\r\n".repeat(500)), true);
await waitForLayer("s:2", (debug) => debug.baseY > 100, "Codex scrollback");
await dispatchWheel(-130);
await waitForLayer("s:2", (debug) => debug.viewportY < debug.baseY, "Codex viewport scroll");
const codexBeforeSwitches = snapshotLayer("s:2");
const preservedScreen = codexBeforeSwitches.screenText.join("\n");

const failures: unknown[] = [];
for (let index = 0; index < 200; index += 1) {
  host.style.width = index % 2 === 0 ? "70%" : "85%";
  stage.show(1n);
  fixture.write("s:2", encoder.encode(`codex hidden ${index}\r\n`.repeat(20)));
  stage.unmount(host);
  stage.show(2n);
  host.style.width = "100%";
  stage.mount(host);
  await frame();
  const debug = layer("s:2");
  const rect = visibleCanvas().getBoundingClientRect();
  const thumb = host.querySelector<HTMLElement>(
    '.term-layer[style*="visibility: visible"] .ghostty-scrollbar-thumb',
  );
  const state = {
    index,
    visible: debug.visible,
    mode: debug.mouseProtocol,
    buffer: debug.bufferType,
    baseY: debug.baseY,
    viewportY: debug.viewportY,
    screen: [rect.width, rect.height],
    thumb: [thumb?.style.height, thumb?.style.top],
  };
  const hostRect = host.getBoundingClientRect();
  const aligned = rect.width >= hostRect.width * 0.9 && rect.height >= hostRect.height * 0.9;
  if (!debug.visible
    || state.mode !== "none"
    || state.buffer !== "normal"
    || debug.screenText.join("\n") !== preservedScreen
    || !aligned
    || !thumb?.style.height) {
    failures.push(state);
  }
}

stage.show(2n);
await dispatchWheel(-4_000);
const beforeWheel = layer("s:2").viewportY;
const beforeInput = inputs.get("s:2") ?? 0;
(window as unknown as { __layoutProbe: unknown; __layoutProbeAfterWheel: () => unknown }).__layoutProbeAfterWheel = () => ({
  afterWheel: layer("s:2").viewportY,
  afterInput: inputs.get("s:2") ?? 0,
  scrollbarAfterWheel: host.querySelector<HTMLElement>(
    '.term-layer[style*="visibility: visible"] .ghostty-scrollbar-thumb',
  )?.style.cssText,
});
(window as unknown as { __layoutProbe: unknown }).__layoutProbe = {
  failures,
  initialGeometry,
  lifecycleRecovery,
  claudeState,
  codexState: snapshotLayer("s:2"),
  beforeWheel,
  beforeInput,
  baseY: layer("s:2").baseY,
  scrollbarBeforeWheel: host.querySelector<HTMLElement>(
    '.term-layer[style*="visibility: visible"] .ghostty-scrollbar-thumb',
  )?.style.cssText,
  resizes: Object.fromEntries(resizes),
};
