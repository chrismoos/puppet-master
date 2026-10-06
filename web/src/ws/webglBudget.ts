import { WebglAddon } from "@xterm/addon-webgl";
import type { Terminal } from "@xterm/xterm";

export const MAX_WEBGL_TERMINALS = 8;
const RETRY_DELAY_MS = 250;
const MAX_RETRY_DELAY_MS = 5_000;
const LOSS_BURST_WINDOW_MS = 1_000;

export type RendererIssue = "budget" | "unavailable" | "context-loss";

export class WebglBudget {
  private active = 0;
  private listeners = new Set<() => void>();

  constructor(private maximum: number) {}

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  acquire(): (() => void) | null {
    if (this.active >= this.maximum) return null;
    this.active += 1;
    let held = true;
    return () => {
      if (!held) return;
      held = false;
      this.active -= 1;
      queueMicrotask(() => {
        for (const listener of this.listeners) listener();
      });
    };
  }
}

const budget = new WebglBudget(MAX_WEBGL_TERMINALS);
const cachedRenderers = new Set<() => void>();

export interface WebglHandle {
  readonly active: boolean;
  dispose(): void;
  setVisible(visible: boolean, retainWhileHidden?: boolean): void;
}

function terminalWebglContext(terminal: Terminal): { canvas: HTMLCanvasElement; gl: WebGL2RenderingContext } | null {
  const canvases = terminal.element?.querySelectorAll<HTMLCanvasElement>(".xterm-screen canvas") ?? [];
  for (const canvas of canvases) {
    const gl = canvas.getContext("webgl2");
    if (gl) return { canvas, gl };
  }
  return null;
}

export function attachWebglRenderer(
  terminal: Terminal,
  onIssue?: (reason: RendererIssue | null) => void,
  initiallyVisible = true,
): WebglHandle {
  const screen = terminal.element?.querySelector<HTMLElement>(".xterm-screen") ?? null;
  const screenVisibility = screen?.style.visibility ?? "";
  const status = terminal.element?.ownerDocument.createElement("div") ?? null;
  if (status) {
    status.className = "terminal-gpu-status";
    status.setAttribute("role", "status");
    status.hidden = true;
    terminal.element?.appendChild(status);
  }
  let visible = initiallyVisible;
  let disposed = false;
  let addon: WebglAddon | null = null;
  let context: ReturnType<typeof terminalWebglContext> = null;
  let release: (() => void) | null = null;
  let retry: ReturnType<typeof setTimeout> | null = null;
  let retryDelay = RETRY_DELAY_MS;
  let lastLoss: number | null = null;
  let issue: RendererIssue | null = null;

  const showIssue = (next: RendererIssue | null) => {
    // xterm installs an HTML renderer during addon disposal, which must never be displayed.
    if (screen) screen.style.visibility = next === null && context ? screenVisibility : "hidden";
    if (status) {
      status.hidden = !visible || next === null;
      status.textContent = next === "budget" ? "Waiting for a GPU renderer…" : "Recovering GPU rendering…";
    }
    if (issue !== next) {
      issue = next;
      onIssue?.(next);
    }
  };
  const evictCached = () => destroyRenderer();
  const destroyRenderer = () => {
    cachedRenderers.delete(evictCached);
    context?.canvas.removeEventListener("webglcontextlost", handleContextLoss, true);
    if (screen) screen.style.visibility = "hidden";
    try {
      addon?.dispose();
    } finally {
      addon = null;
      // Detached canvases still count toward the browser's live-context limit until collection.
      if (context && !context.gl.isContextLost()) context.gl.getExtension("WEBGL_lose_context")?.loseContext();
      context = null;
      release?.();
      release = null;
    }
  };
  const scheduleRetry = () => {
    if (disposed || !visible || retry !== null) return;
    retry = setTimeout(() => {
      retry = null;
      activate();
    }, retryDelay);
    retryDelay = Math.min(retryDelay * 2, MAX_RETRY_DELAY_MS);
  };
  const activate = () => {
    if (disposed || !visible || addon || retry !== null) return;
    release = budget.acquire();
    if (!release) {
      cachedRenderers.values().next().value?.();
      release = budget.acquire();
    }
    if (!release) {
      showIssue("budget");
      return;
    }
    try {
      const nextAddon = new WebglAddon();
      addon = nextAddon;
      addon.onContextLoss(() => {
        if (addon === nextAddon) handleContextLoss();
      });
      terminal.loadAddon(addon);
      context = terminalWebglContext(terminal);
      if (!context || context.gl.isContextLost()) throw new Error("WebGL renderer is unavailable");
      // Capture replaces the renderer before xterm's restoration timer and stale-resource cleanup.
      context.canvas.addEventListener("webglcontextlost", handleContextLoss, true);
      showIssue(null);
      terminal.refresh(0, terminal.rows - 1);
      retryDelay = RETRY_DELAY_MS;
    } catch {
      context ??= terminalWebglContext(terminal);
      destroyRenderer();
      showIssue("unavailable");
      scheduleRetry();
    }
  };
  const handleContextLoss = () => {
    if (disposed || !addon) return;
    const now = performance.now();
    const repeated = lastLoss !== null && now - lastLoss < LOSS_BURST_WINDOW_MS;
    lastLoss = now;
    showIssue("context-loss");
    destroyRenderer();
    if (repeated) scheduleRetry();
    else activate();
  };
  const unsubscribe = budget.subscribe(activate);
  const cancelRetry = () => {
    if (retry !== null) clearTimeout(retry);
    retry = null;
  };
  const setVisible = (next: boolean, retainWhileHidden = false) => {
    if (disposed) return;
    visible = next;
    cachedRenderers.delete(evictCached);
    if (next) activate();
    else {
      cancelRetry();
      if (retainWhileHidden && addon) cachedRenderers.add(evictCached);
      else destroyRenderer();
      showIssue(null);
    }
  };
  showIssue(null);
  if (visible) activate();
  return {
    get active() { return context !== null && !context.gl.isContextLost(); },
    setVisible,
    dispose() {
      if (disposed) return;
      disposed = true;
      unsubscribe();
      cancelRetry();
      destroyRenderer();
      status?.remove();
    },
  };
}
