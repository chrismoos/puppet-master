import { TerminalStage } from "../src/ws/terminal";
import { TerminalThemeController } from "../src/theme/controller";
import type { PmClient, PtyHandle } from "../src/ws/client";
import type { PtySink } from "../src/ws/pty";
import "@xterm/xterm/css/xterm.css";
import "../src/styles.css";

const frame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
const encoder = new TextEncoder();
const sinks = new Map<string, PtySink>();
const inputs = new Map<string, number>();
const resizes = new Map<string, string[]>();
const handles = (id: bigint): PtyHandle => ({
  connect: (sink) => sinks.set(id.toString(), sink),
  input: (data) => inputs.set(id.toString(), (inputs.get(id.toString()) ?? 0) + data.byteLength),
  resize: (cols, rows) => {
    const entries = resizes.get(id.toString()) ?? [];
    entries.push(`${cols}x${rows}`);
    resizes.set(id.toString(), entries);
  },
  onStatus: (listener) => { listener({ phase: "online" }); return () => {}; },
  ptySize: () => null,
  onPtySize: () => () => {},
  retry: () => {},
  close: () => {},
});
const client = {
  openPty: (id: bigint) => handles(id),
  openTerminal: (id: bigint) => handles(id),
} as unknown as PmClient;

const host = document.querySelector<HTMLElement>("#host")!;
Object.assign(document.body.style, { margin: "0", width: "100vw", height: "100vh", overflow: "hidden" });
Object.assign(host.style, { position: "relative", width: "0", height: "0" });

// Make layout-transition recovery deterministic. Browsers normally deliver a
// ResizeObserver notification here, but the production race is precisely that
// attach/show can settle before a useful delivery. TerminalStage must also own
// an explicit post-layout reconciliation for that lifecycle transition.
class InertResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}
window.ResizeObserver = InertResizeObserver as unknown as typeof ResizeObserver;

const stage = new TerminalStage(client, new TerminalThemeController());
stage.mount(host);
stage.show(1n);
Object.assign(host.style, { width: "100%", height: "100%" });
await frame();
await frame();

type DebugLayer = {
  el: HTMLDivElement;
  term: import("@xterm/xterm").Terminal;
};
const debug = stage as unknown as { layers: Map<string, DebugLayer>; visibleId: string | null };
const recovered = debug.layers.get("s:1")!;
const lifecycleRecovery = {
  cols: recovered.term.cols,
  rows: recovered.term.rows,
  screen: (() => {
    const rect = recovered.term.element!.querySelector<HTMLElement>(".xterm-screen")!.getBoundingClientRect();
    return [rect.width, rect.height];
  })(),
};

sinks.get("1")!({ data: encoder.encode("claude normal\r\n".repeat(500) + "\x1b[?1049h\x1b[?1002h\x1b[?1006hCLAUDE ALT"), replay: true });
await frame();
await frame();
const claudeState = {
  mode: recovered.term.modes.mouseTrackingMode,
  buffer: recovered.term.buffer.active.type,
};
stage.show(2n);
sinks.get("2")!({ data: encoder.encode("codex native scrollback\r\n".repeat(500)), replay: true });
await frame();
await frame();
const codexBeforeSwitches = debug.layers.get("s:2")!;
codexBeforeSwitches.term.scrollToLine(Math.max(0, codexBeforeSwitches.term.buffer.active.baseY - 100));
await frame();
const preservedViewportY = codexBeforeSwitches.term.buffer.active.viewportY;
const failures: unknown[] = [];
for (let index = 0; index < 200; index += 1) {
  host.style.width = index % 2 === 0 ? "70%" : "85%";
  stage.show(1n);
  sinks.get("2")!({ data: encoder.encode(`codex hidden ${index}\r\n`.repeat(20)), replay: false });
  stage.unmount(host);
  stage.show(2n);
  host.style.width = "100%";
  stage.mount(host);
  await frame();
  const layer = debug.layers.get("s:2")!;
  const xterm = layer.term.element!;
  const screen = xterm.querySelector<HTMLElement>(".xterm-screen")!;
  const slider = xterm.querySelector<HTMLElement>(".scrollbar.vertical .slider")!;
  const hostRect = host.getBoundingClientRect();
  const screenRect = screen.getBoundingClientRect();
  const state = {
    index,
    visibleId: debug.visibleId,
    visibleLayers: [...debug.layers.values()].filter((candidate) => getComputedStyle(candidate.el).visibility === "visible").length,
    mode: layer.term.modes.mouseTrackingMode,
    buffer: layer.term.buffer.active.type,
    baseY: layer.term.buffer.active.baseY,
    viewportY: layer.term.buffer.active.viewportY,
    host: [hostRect.width, hostRect.height],
    screen: [screenRect.width, screenRect.height],
    slider: [slider.style.height, slider.style.top],
  };
  const aligned = screenRect.width >= hostRect.width * 0.9
    && screenRect.height >= hostRect.height * 0.9;
  if (state.visibleId !== "s:2"
    || state.visibleLayers !== 1
    || state.mode !== "none"
    || state.buffer !== "normal"
    || state.viewportY !== preservedViewportY
    || !aligned
    || !slider.style.height) {
    failures.push(state);
  }
}
const codex = debug.layers.get("s:2")!;
codex.term.scrollToTop();
await frame();
const beforeWheel = codex.term.buffer.active.viewportY;
const beforeInput = inputs.get("2") ?? 0;
(window as unknown as { __layoutProbe: unknown; __layoutProbeAfterWheel: () => unknown }).__layoutProbeAfterWheel = () => ({
  afterWheel: codex.term.buffer.active.viewportY,
  afterInput: inputs.get("2") ?? 0,
  sliderAfterWheel: codex.term.element!.querySelector<HTMLElement>(".scrollbar.vertical .slider")!.style.cssText,
});
(window as unknown as { __layoutProbe: unknown }).__layoutProbe = {
  failures,
  lifecycleRecovery,
  claudeState,
  codexState: {
    mode: codex.term.modes.mouseTrackingMode,
    buffer: codex.term.buffer.active.type,
  },
  beforeWheel,
  beforeInput,
  baseY: codex.term.buffer.active.baseY,
  sliderBeforeWheel: codex.term.element!.querySelector<HTMLElement>(".scrollbar.vertical .slider")!.style.cssText,
  resizes: Object.fromEntries(resizes),
};
