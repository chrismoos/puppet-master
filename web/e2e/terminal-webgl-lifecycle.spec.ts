import type { Terminal } from "@xterm/xterm";
import type { TerminalStage } from "../src/ws/terminal";
import { expect, test, type Locator, type Page } from "./fixtures";
import { logIn } from "./support";

const CONTEXT_CHURN_COUNT = 24;
const TAB_VISIBILITY_CYCLES = 3;
const RESTORATION_WINDOW_MS = 3_200;
const VISIBLE_TERMINAL = '.term-layer[style*="visible"]';

type ProbeWindow = Window & {
  __pmStage: {
    debugSnapshot: TerminalStage["debugSnapshot"];
    disposeSession: TerminalStage["disposeSession"];
    layers: Map<string, { term: Terminal }>;
  };
  __webglProbe: {
    contexts: WebGL2RenderingContext[];
    socketOpens: number;
    socketCloses: number;
    output: string;
  };
};

async function openWithProbe(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const probe = { contexts: [] as WebGL2RenderingContext[], socketOpens: 0, socketCloses: 0, output: "" };
    (window as ProbeWindow).__webglProbe = probe;
    const nativeGetContext = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = new Proxy(nativeGetContext, {
      apply(target, receiver, args) {
        const context = Reflect.apply(target, receiver, args);
        if (args[0] === "webgl2" && context && !probe.contexts.includes(context)) {
          probe.contexts.push(context);
        }
        return context;
      },
    });
    window.WebSocket = new Proxy(window.WebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        if (String(args[0]).includes("/ws/terminal/")) {
          probe.socketOpens += 1;
          socket.addEventListener("close", () => probe.socketCloses += 1);
          const decoder = new TextDecoder();
          const OUTPUT_TAG = 0x01;
          const OUTPUT_HEADER_BYTES = 10;
          socket.addEventListener("message", (event) => {
            if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < OUTPUT_HEADER_BYTES) return;
            if (new DataView(event.data).getUint8(0) !== OUTPUT_TAG) return;
            probe.output += decoder.decode(new Uint8Array(event.data, OUTPUT_HEADER_BYTES), { stream: true });
          });
        }
        return socket;
      },
    });
  });
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expect(page.locator(`${VISIBLE_TERMINAL} .xterm`)).toBeVisible();
}

async function echo(page: Page, terminal: Locator, text: string): Promise<void> {
  await terminal.locator(".xterm-helper-textarea").focus();
  await page.keyboard.type(`echo ${text}`);
  await page.keyboard.press("Enter");
}

async function loseContext(terminal: Locator): Promise<{ domAtLoss: boolean; gpuAtLoss: boolean }> {
  return terminal.evaluate((host) => {
    const gl = Array.from(host.querySelectorAll(".xterm-screen canvas"))
      .map((canvas) => (canvas as HTMLCanvasElement).getContext("webgl2"))
      .find((context) => context !== null);
    if (!gl) throw new Error("terminal has no WebGL context");
    const extension = gl.getExtension("WEBGL_lose_context");
    if (!extension) throw new Error("context-loss simulation is unavailable");
    return new Promise<{ domAtLoss: boolean; gpuAtLoss: boolean }>((resolve) => {
      (gl.canvas as HTMLCanvasElement).addEventListener("webglcontextlost", () => {
        const current = Array.from(host.querySelectorAll(".xterm-screen canvas"))
          .map((canvas) => (canvas as HTMLCanvasElement).getContext("webgl2")).find(Boolean);
        resolve({ domAtLoss: Boolean(host.querySelector(".xterm-rows")), gpuAtLoss: Boolean(current && !current.isContextLost()) });
      }, { once: true });
      extension.loseContext();
    });
  });
}

function visibleText(page: Page): Promise<string> {
  return page.evaluate(() => {
    const stage = (window as ProbeWindow).__pmStage;
    const key = stage.debugSnapshot().find((entry) => entry.visible)!.key;
    const terminal = stage.layers.get(key)!.term;
    const buffer = terminal.buffer.active;
    return Array.from({ length: terminal.rows }, (_, row) =>
      buffer.getLine(buffer.baseY + row)?.translateToString(true) ?? "").join("\n");
  });
}

function socketCounts(page: Page): Promise<{ opens: number; closes: number }> {
  return page.evaluate(() => {
    const probe = (window as ProbeWindow).__webglProbe;
    return { opens: probe.socketOpens, closes: probe.socketCloses };
  });
}

test("terminal churn releases hidden contexts and keeps the selected terminal GPU-rendered", async ({ page }) => {
  const warnings: string[] = [];
  page.on("console", (message) => warnings.push(message.text()));
  await openWithProbe(page);
  const keeperKey = await page.evaluate(() =>
    (window as ProbeWindow).__pmStage.debugSnapshot().find((layer) => layer.visible)!.key);
  const sessions = page.locator(".sb-session");

  for (let index = 0; index < CONTEXT_CHURN_COUNT; index += 1) {
    await sessions.nth(1).click();
    await expect(sessions.nth(1)).toHaveClass(/is-selected/);
    await expect(page.locator(`${VISIBLE_TERMINAL} .xterm`)).toBeVisible();
    await sessions.nth(0).click();
    await expect(sessions.nth(0)).toHaveClass(/is-selected/);
    await page.evaluate((key) => {
      const stage = (window as ProbeWindow).__pmStage;
      for (const layer of stage.debugSnapshot()) {
        if (layer.key !== key) stage.disposeSession(BigInt(layer.key.slice(2)));
      }
    }, keeperKey);
  }

  const contexts = await page.evaluate(() => {
    const probe = (window as ProbeWindow).__webglProbe;
    return {
      created: probe.contexts.length,
      selectedGpu: Array.from(document.querySelectorAll('.term-layer[style*="visible"] .xterm-screen canvas'))
        .map((canvas) => (canvas as HTMLCanvasElement).getContext("webgl2"))
        .some((gl) => gl !== null && !gl.isContextLost()),
      detachedLive: probe.contexts.filter((gl) =>
        !(gl.canvas as HTMLCanvasElement).isConnected && !gl.isContextLost()).length,
    };
  });
  expect(contexts.created).toBeGreaterThanOrEqual(CONTEXT_CHURN_COUNT + 1);
  expect(contexts.detachedLive).toBe(0);
  expect(contexts.selectedGpu).toBe(true);
  expect(warnings.filter((message) => /Too many active WebGL|webglcontextlost event|INVALID_OPERATION/.test(message)))
    .toEqual([]);
});

test("context loss preserves visible output and input without a replay or reconnect", async ({ page }) => {
  const warnings: string[] = [];
  page.on("console", (message) => warnings.push(message.text()));
  await openWithProbe(page);
  const terminal = page.locator(VISIBLE_TERMINAL);
  await page.evaluate(() => {
    const stage = (window as ProbeWindow).__pmStage;
    const key = stage.debugSnapshot().find((entry) => entry.visible)!.key;
    stage.layers.get(key)!.term.options.cursorBlink = false;
  });
  await echo(page, terminal, "before-context-loss");
  await expect.poll(() => visibleText(page)).toContain("OUT before-context-loss");
  const image = await terminal.locator(".xterm-screen canvas:not(.xterm-link-layer)").screenshot();
  const before = await page.evaluate(() => {
    const layer = (window as ProbeWindow).__pmStage.debugSnapshot().find((entry) => entry.visible)!;
    return { generation: layer.socket!.generation, replayCount: layer.socket!.replayCount };
  });
  const sockets = await socketCounts(page);

  expect(await loseContext(terminal)).toEqual({ domAtLoss: false, gpuAtLoss: true });
  await expect.poll(async () => Buffer.compare(
    await terminal.locator(".xterm-screen canvas:not(.xterm-link-layer)").screenshot(), image,
  )).toBe(0);
  await echo(page, terminal, "after-context-loss");
  await expect.poll(() => visibleText(page)).toContain("OUT after-context-loss");
  const recovered = await page.evaluate(() => {
    const layer = (window as ProbeWindow).__pmStage.debugSnapshot().find((entry) => entry.visible)!;
    return { generation: layer.socket!.generation, replayCount: layer.socket!.replayCount };
  });
  expect(recovered).toEqual(before);

  await page.getByRole("button", { name: "info", exact: true }).click();
  await page.getByRole("button", { name: "info", exact: true }).click();
  await expect.poll(() => visibleText(page)).toContain("OUT after-context-loss");
  // e2e-real-time-wait: a disposed renderer must stay quiet past xterm's three-second restoration deadline.
  await page.waitForTimeout(RESTORATION_WINDOW_MS);
  expect(await socketCounts(page)).toEqual(sockets);
  const generation = await page.evaluate(() =>
    (window as ProbeWindow).__pmStage.debugSnapshot().find((entry) => entry.visible)!.socket!.generation);
  expect(generation).toBe(before.generation);
  expect(warnings.filter((message) => /webglcontextlost event|webglcontextrestored event|context not restored|INVALID_OPERATION/.test(message)))
    .toEqual([]);
});

test("workspace terminals recreate WebGL and keep accepting input", async ({ page }) => {
  await openWithProbe(page);
  await page.getByTitle("new workspace").click();
  await page.getByRole("button", { name: "create workspace" }).click();
  await page.getByLabel("terminal shown in pane").selectOption({ label: "browser-e2e · agent" });
  const terminal = page.locator(".workspace-terminal-host");
  await expect(terminal.locator(".xterm")).toBeVisible();
  const sockets = await socketCounts(page);

  expect(await loseContext(terminal)).toEqual({ domAtLoss: false, gpuAtLoss: true });
  await expect(page.locator(".workspace-renderer-warning")).toHaveCount(0);
  await echo(page, terminal, "workspace-after-loss");
  await expect.poll(() => page.evaluate(() => (window as ProbeWindow).__webglProbe.output))
    .toContain("OUT workspace-after-loss");
  await expect(terminal.locator(".xterm-screen canvas:not(.xterm-link-layer)")).toBeVisible();
  await expect(terminal.locator(".xterm-rows")).toHaveCount(0);
  expect(await socketCounts(page)).toEqual(sockets);
});

async function verifyTabVisibilityPreservesContexts(page: Page): Promise<void> {
  const before = await page.evaluate(() => {
    const probe = (window as ProbeWindow).__webglProbe;
    return { created: probe.contexts.length, live: probe.contexts.filter((gl) => !gl.isContextLost()).length };
  });
  expect(before.live).toBeGreaterThan(0);
  for (let cycle = 0; cycle < TAB_VISIBILITY_CYCLES; cycle += 1) {
    for (const state of ["hidden", "visible"]) {
      const after = await page.evaluate(async (visibilityState) => {
        Object.defineProperty(document, "visibilityState", { configurable: true, value: visibilityState });
        document.dispatchEvent(new Event("visibilitychange"));
        await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
        const probe = (window as ProbeWindow).__webglProbe;
        return { created: probe.contexts.length, live: probe.contexts.filter((gl) => !gl.isContextLost()).length };
      }, state);
      expect(after).toEqual(before);
    }
  }
  await page.evaluate(() => Reflect.deleteProperty(document, "visibilityState"));
}

test("browser tab visibility preserves selected and cached session GPU contexts", async ({ page }) => {
  await openWithProbe(page);
  const sessions = page.locator(".sb-session");
  await sessions.nth(1).click();
  await expect(sessions.nth(1)).toHaveClass(/is-selected/);
  await expect(page.locator(`${VISIBLE_TERMINAL} .xterm`)).toBeVisible();
  await sessions.nth(0).click();
  await expect(sessions.nth(0)).toHaveClass(/is-selected/);
  await verifyTabVisibilityPreservesContexts(page);
  await expect(page.locator(`${VISIBLE_TERMINAL} .xterm-rows`)).toHaveCount(0);
  await echo(page, page.locator(VISIBLE_TERMINAL), "session-after-tab-switch");
  await expect.poll(() => visibleText(page)).toContain("OUT session-after-tab-switch");
});

test("browser tab visibility preserves workspace GPU contexts", async ({ page }) => {
  await openWithProbe(page);
  await page.getByTitle("new workspace").click();
  await page.getByRole("button", { name: "create workspace" }).click();
  await page.getByLabel("terminal shown in pane").selectOption({ label: "browser-e2e · agent" });
  const terminal = page.locator(".workspace-terminal-host");
  await expect(terminal.locator(".xterm")).toBeVisible();
  await verifyTabVisibilityPreservesContexts(page);
  await expect(terminal.locator(".xterm-rows")).toHaveCount(0);
  await echo(page, terminal, "workspace-after-tab-switch");
  await expect.poll(() => page.evaluate(() => (window as ProbeWindow).__webglProbe.output))
    .toContain("OUT workspace-after-tab-switch");
});
